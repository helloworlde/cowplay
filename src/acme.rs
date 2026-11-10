//! DDNS providers and DNS-01 issuance. HTTP, JWS, CSR and ACME state handling are
//! delegated to reqwest/instant-acme/rcgen; only provider orchestration lives here.
use axum::{body::Bytes, http::{HeaderMap, Request}};
use http_body_util::{BodyExt, Full};
use instant_acme::{Account, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus, RetryPolicy};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};

pub type Result<T> = std::result::Result<T, String>;
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const CA_FILES: [&str; 3] = ["/etc/ssl/certs/ca-certificates.crt", "/etc/ssl/cert.pem", "/etc/ssl/certs/ca-bundle.crt"];
fn request_error(err: reqwest::Error) -> String {
    // DuckDNS puts its token in the query. Never include request URLs in errors/logs.
    err.without_url().to_string()
}
async fn client() -> Result<reqwest::Client> {
    static CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();
    CLIENT.get_or_try_init(|| async {
        tokio::task::spawn_blocking(|| {
            use rustls_pki_types::{CertificateDer, pem::PemObject};
            let mut roots = rustls::RootCertStore::empty();
            for path in CA_FILES {
                if let Ok(certs) = CertificateDer::pem_file_iter(path) {
                    for cert in certs.flatten() { let _ = roots.add(cert); }
                }
            }
            if roots.is_empty() { return Err("未找到系统 CA 证书（需要 ca-bundle）".into()); }
            let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions().map_err(|e| e.to_string())?
                .with_root_certificates(roots).with_no_client_auth();
            reqwest::Client::builder().https_only(true).no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(25)).connect_timeout(Duration::from_secs(10))
                .pool_max_idle_per_host(1).pool_idle_timeout(Duration::from_secs(30))
                .user_agent("CowPlay").use_preconfigured_tls(tls)
                .build().map_err(request_error)
        }).await.map_err(|e| e.to_string())?
    }).await.cloned()
}
pub struct Response {
    pub status: u16,
    headers: HeaderMap,
    pub body: Vec<u8>,
}
impl Response {
    pub fn header(&self, name: &str) -> Option<&str> { self.headers.get(name)?.to_str().ok() }
    pub fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.body).map_err(|_| format!("响应不是 JSON（HTTP {}）", self.status))
    }
    pub fn text(&self) -> String { String::from_utf8_lossy(&self.body).chars().take(300).collect() }
}
async fn fetch(request: reqwest::RequestBuilder) -> Result<Response> {
    let mut response = request.send().await.map_err(request_error)?;
    if response.content_length().is_some_and(|n| n > RESPONSE_LIMIT as u64) { return Err("响应过大".into()); }
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(request_error)? {
        if chunk.len() > RESPONSE_LIMIT - body.len() { return Err("响应过大".into()); }
        body.extend_from_slice(&chunk);
    }
    Ok(Response { status, headers, body })
}
pub async fn https(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Result<Response> {
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let mut request = client().await?.request(method, url);
    for (name, value) in headers { request = request.header(*name, *value); }
    if let Some(body) = body { request = request.body(body.to_vec()); }
    fetch(request).await
}
/// The same bounded, pooled HTTP client is used for ACME and DNS provider requests.
struct AcmeHttp(reqwest::Client);
impl instant_acme::HttpClient for AcmeHttp {
    fn request(&self, request: Request<instant_acme::BodyWrapper<Bytes>>) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::result::Result<instant_acme::BytesResponse, instant_acme::Error>> + Send>> {
        let client = self.0.clone();
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let body = body.collect().await.expect("ACME byte body is infallible").to_bytes();
            let response = fetch(client.request(parts.method, parts.uri.to_string()).headers(parts.headers).body(body))
                .await.map_err(|e| instant_acme::Error::Other(Box::new(std::io::Error::other(e))))?;
            let mut result = axum::http::Response::builder().status(response.status).body(Full::new(Bytes::from(response.body)))?;
            *result.headers_mut() = response.headers;
            Ok(result.into())
        })
    }
}

// ---------------- DNS providers ----------------

#[derive(Clone)]
pub enum Provider {
    Cloudflare { token: String },
    DuckDns { token: String },
}
fn duck_sub(domain: &str) -> Result<&str> {
    domain
        .strip_suffix(".duckdns.org")
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .ok_or_else(|| "DuckDNS 域名必须是 <名称>.duckdns.org".into())
}
fn query_escape(v: &str) -> String {
    v.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}
impl Provider {
    async fn cf(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let Provider::Cloudflare { token } = self else { unreachable!() };
        let auth = format!("Bearer {token}");
        let payload = body.map(|b| b.to_string());
        let r = https(method, &format!("https://api.cloudflare.com/client/v4{path}"), &[("Authorization", &auth), ("Content-Type", "application/json")], payload.as_deref().map(str::as_bytes)).await?;
        let v = r.json()?;
        if v["success"] != true {
            return Err(format!("Cloudflare 拒绝（HTTP {}）：{}", r.status, v["errors"]));
        }
        Ok(v)
    }
    async fn cf_zone(&self, domain: &str) -> Result<String> {
        let labels: Vec<&str> = domain.split('.').collect();
        for start in 0..labels.len().saturating_sub(1) {
            let zone = labels[start..].join(".");
            let v = self.cf("GET", &format!("/zones?name={}", query_escape(&zone)), None).await?;
            if let Some(id) = v["result"][0]["id"].as_str() {
                return Ok(id.to_owned());
            }
        }
        Err(format!("Cloudflare 账号下没有 {domain} 所在的区域（检查令牌权限）"))
    }
    async fn duck(&self, query: &str) -> Result<String> {
        let Provider::DuckDns { token } = self else { unreachable!() };
        let r = https("GET", &format!("https://www.duckdns.org/update?{query}&token={}", query_escape(token)), &[], None).await?;
        let text = r.text();
        if r.status != 200 || !text.starts_with("OK") {
            return Err(format!("DuckDNS 返回 {}（检查域名与令牌）", text.lines().next().unwrap_or("")));
        }
        Ok(text)
    }
    /// Point `domain` at `ip`. Returns a human-readable outcome.
    pub async fn set_a(&self, domain: &str, ip: &str) -> Result<String> {
        match self {
            Provider::DuckDns { .. } => {
                self.duck(&format!("domains={}&ip={}", duck_sub(domain)?, query_escape(ip))).await?;
                Ok(format!("DuckDNS {domain} → {ip}"))
            }
            Provider::Cloudflare { .. } => {
                let zone = self.cf_zone(domain).await?;
                let found = self.cf("GET", &format!("/zones/{zone}/dns_records?type=A&name={}", query_escape(domain)), None).await?;
                match found["result"][0].as_object() {
                    Some(r) if r["content"] == ip => Ok(format!("Cloudflare {domain} 已是 {ip}，无需更新")),
                    Some(r) => {
                        let id = r["id"].as_str().unwrap_or_default();
                        self.cf("PATCH", &format!("/zones/{zone}/dns_records/{id}"), Some(json!({"content": ip}))).await?;
                        Ok(format!("Cloudflare {domain} → {ip}（已更新）"))
                    }
                    None => {
                        self.cf("POST", &format!("/zones/{zone}/dns_records"), Some(json!({"type":"A","name":domain,"content":ip,"ttl":60,"proxied":false}))).await?;
                        Ok(format!("Cloudflare {domain} → {ip}（已创建）"))
                    }
                }
            }
        }
    }
    async fn set_txt(&self, domain: &str, value: &str) -> Result<Option<(String, String)>> {
        match self {
            Provider::DuckDns { .. } => {
                self.duck(&format!("domains={}&txt={}", duck_sub(domain)?, query_escape(value))).await?;
                Ok(None)
            }
            Provider::Cloudflare { .. } => {
                let zone = self.cf_zone(domain).await?;
                let v = self.cf("POST", &format!("/zones/{zone}/dns_records"), Some(json!({"type":"TXT","name":format!("_acme-challenge.{domain}"),"content":value,"ttl":60}))).await?;
                Ok(v["result"]["id"].as_str().map(|id| (zone, id.to_owned())))
            }
        }
    }
    async fn clear_txt(&self, domain: &str, record: Option<(String, String)>) {
        let _ = match (self, record) {
            (Provider::DuckDns { .. }, _) => match duck_sub(domain) {
                Ok(sub) => self.duck(&format!("domains={sub}&txt=removed&clear=true")).await.map(|_| ()),
                Err(err) => Err(err),
            },
            (Provider::Cloudflare { .. }, Some((zone, id))) => self.cf("DELETE", &format!("/zones/{zone}/dns_records/{id}"), None).await.map(|_| ()),
            _ => Ok(()),
        };
    }
}

pub const LETSENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";
pub const LETSENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";
pub struct Issued { pub chain_pem: String, pub key_pem: String }

async fn account(http: reqwest::Client, directory: &str, email: &str, legacy: &Path, staging: bool) -> Result<Account> {
    use rustls_pki_types::{PrivatePkcs8KeyDer, pem::PemObject};
    // Staging and production accounts have separate persistent identities.
    let path = legacy.with_extension(if staging { "staging.json" } else { "json" });
    let builder = Account::builder_with_http(Box::new(AcmeHttp(http)));
    if path.exists() {
        let saved = f50_server::settings::read(path.to_str().ok_or("ACME 账号路径不是 UTF-8")?, 32768).map_err(|e| e.to_string())?;
        return builder.from_credentials(serde_json::from_slice(&saved).map_err(|e| format!("ACME 账号文件无效：{e}"))?)
            .await.map_err(|e| e.to_string());
    }
    let (account, credentials) = if legacy.exists() && !staging {
        // Preserve the original account key; never silently replace an unreadable key.
        let der = PrivatePkcs8KeyDer::from_pem_file(legacy).map_err(|e| e.to_string())?;
        let key = instant_acme::Key::from_pkcs8_der(der.clone_key()).map_err(|e| e.to_string())?;
        builder.create_from_key((key, rustls_pki_types::PrivateKeyDer::Pkcs8(der)), directory.to_owned()).await
    } else {
        let contact = (!email.is_empty()).then(|| format!("mailto:{email}"));
        let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
        builder.create(&NewAccount { contact: &contacts, terms_of_service_agreed: true, only_return_existing: false }, directory.to_owned(), None).await
    }.map_err(|e| e.to_string())?;
    f50_server::settings::write_private(&path, &serde_json::to_vec(&credentials).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(account)
}
pub async fn issue(domain: &str, provider: &Provider, email: &str, staging: bool, account_key_path: &Path, progress: &(dyn Fn(&str) + Sync)) -> Result<Issued> {
    progress("恢复 ACME 账号");
    let account = account(client().await?, if staging { LETSENCRYPT_STAGING } else { LETSENCRYPT }, email, account_key_path, staging).await?;
    let ids = [Identifier::Dns(domain.to_owned())];
    let mut order = account.new_order(&NewOrder::new(&ids)).await.map_err(|e| e.to_string())?;
    let mut records = Vec::new();
    let result = async {
        let mut auths = order.authorizations();
        while let Some(auth) = auths.next().await {
            let mut auth = auth.map_err(|e| e.to_string())?;
            match auth.status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {},
                _ => return Err("域名授权不可用".into()),
            }
            let mut challenge = auth.challenge(ChallengeType::Dns01).ok_or("没有 DNS-01 挑战")?;
            progress("写入 _acme-challenge TXT 记录");
            let record = provider.set_txt(domain, &challenge.key_authorization().dns_value()).await?;
            records.push(record);
            progress("等待 DNS 生效（约 60 秒）");
            tokio::time::sleep(Duration::from_secs(60)).await;
            challenge.set_ready().await.map_err(|e| e.to_string())?;
        }
        progress("等待 Let's Encrypt 校验");
        let retry = RetryPolicy::new().initial_delay(Duration::from_secs(3)).timeout(Duration::from_secs(120));
        if order.poll_ready(&retry).await.map_err(|e| e.to_string())? != OrderStatus::Ready { return Err("证书订单尚未就绪".into()); }
        progress("提交证书请求");
        let key_pem = order.finalize().await.map_err(|e| e.to_string())?;
        let chain_pem = order.poll_certificate(&retry).await.map_err(|e| e.to_string())?;
        Ok(Issued { chain_pem, key_pem })
    }.await;
    for record in records { provider.clear_txt(domain, record).await; }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duckdns_names_are_validated_and_queries_escaped() {
        assert_eq!(duck_sub("my-car.duckdns.org").unwrap(), "my-car");
        assert!(duck_sub("carplay.example.test").is_err());
        assert!(duck_sub("a&b.duckdns.org").is_err());
        assert_eq!(query_escape("a b&c"), "a%20b%26c");
    }
    #[tokio::test]
    async fn http_framing_completes_without_waiting_for_peer_close() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _request = socket.read(&mut request).await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n7\r\n{\"a\":1}\r\n0\r\n\r\n").await.unwrap();
            std::future::pending::<()>().await;
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let response = tokio::time::timeout(Duration::from_secs(1), fetch(client.get(&url))).await.unwrap().unwrap();
        assert_eq!(response.json().unwrap()["a"], 1);
        server.abort();
    }
    #[tokio::test]
    async fn account_uses_bounded_http_and_restores_saved_credentials() {
        use axum::{routing::{get, head, post}, response::IntoResponse};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let registrations = Arc::new(AtomicUsize::new(0));
        let directory = json!({"newNonce":format!("{base}/nonce"),"newAccount":format!("{base}/account"),"newOrder":format!("{base}/order")});
        let account_url = format!("{base}/account/1");
        let count = registrations.clone();
        let app = axum::Router::new()
            .route("/directory", get(move || { let v=directory.clone(); async move { axum::Json(v) } }))
            .route("/nonce", head(|| async { [("replay-nonce", "synthetic-nonce")] }))
            .route("/account", post(move || { let url=account_url.clone(); let count=count.clone(); async move {
                count.fetch_add(1, Ordering::SeqCst);
                (axum::http::StatusCode::CREATED, [("location", url)], axum::Json(json!({"status":"valid"}))).into_response()
            } }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("acme-account.pem");
        let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build().unwrap();
        let first = account(http.clone(), &format!("{base}/directory"), "", &legacy, false).await.unwrap();
        let second = account(http, &format!("{base}/directory"), "", &legacy, false).await.unwrap();
        assert_eq!(first.id(), second.id());
        assert_eq!(first.key_thumbprint(), second.key_thumbprint());
        assert_eq!(registrations.load(Ordering::SeqCst), 1, "restart restores the existing account");
        assert!(!legacy.exists(), "no legacy key is overwritten or invented");
        server.abort();
    }
    #[tokio::test]
    #[ignore = "requires external network; does not create an account or certificate"]
    async fn live_staging_directory_and_nonce() {
        let dir = https("GET", LETSENCRYPT_STAGING, &[], None).await.unwrap().json().unwrap();
        let nonce = https("HEAD", dir["newNonce"].as_str().unwrap(), &[], None).await.unwrap();
        assert!(nonce.header("Replay-Nonce").is_some());
    }
}
