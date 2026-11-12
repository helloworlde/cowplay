use super::{Admin, DdnsPrefs, local_addresses, tls_info};
use f50_server::diag::{self, diag};
use serde_json::json;
use std::{path::{Path, PathBuf}, sync::Arc, time::Duration};

impl Admin {
    pub(crate) fn token_path(&self) -> PathBuf {
        self.state_dir.join("ddns-token")
    }
    pub(crate) fn provider(&self, p: &DdnsPrefs) -> Result<crate::acme::Provider, String> {
        let token = std::fs::read_to_string(self.token_path()).map(|t| t.trim().to_owned()).unwrap_or_default();
        if token.is_empty() {
            return Err("尚未设置证书 DNS API 令牌（与系统 DDNS 凭据分别管理）".into());
        }
        Ok(match p.provider.as_str() {
            "duckdns" => crate::acme::Provider::DuckDns { token },
            _ => crate::acme::Provider::Cloudflare { token },
        })
    }
    pub(crate) fn ddns_ip(&self, p: &DdnsPrefs) -> Option<String> {
        if !p.ip.is_empty() {
            return Some(p.ip.clone());
        }
        let ip = self.web_bind.ip();
        if !ip.is_unspecified() {
            return Some(ip.to_string());
        }
        local_addresses().into_iter().find(|a| a.is_ipv4() && !a.is_loopback()).map(|a| a.to_string())
    }
    pub async fn ddns_update(&self) -> Result<String, String> {
        let p = self.prefs().ddns;
        let provider = self.provider(&p)?;
        let ip = self.ddns_ip(&p).ok_or("无法确定本机地址")?;
        let result = provider.set_a(&p.domain, &ip).await;
        let mut s = self.ddns_state.lock().unwrap();
        s.last_update_ms = Some(diag::now_ms());
        s.update_ok = Some(result.is_ok());
        s.last_update = Some(match &result { Ok(m) => m.clone(), Err(err) => err.clone() });
        drop(s);
        match &result {
            Ok(m) => f50_server::diag_log!(Info, "web", "DDNS {m}"),
            Err(err) => f50_server::diag_log!(Warn, "web", "DDNS update failed: {err}"),
        }
        result
    }
    /// Callers hold ddns_busy. Select the renewal mode before serving a new pair,
    /// restore it on activation failure, and never report stale in-memory TLS paths.
    pub(crate) async fn activate_certificate_pair(&self, cert: &Path, key: &Path, source: &str) -> Result<(), String> {
        if !["acme", "upload", "self", "http"].contains(&source) { return Err("证书来源无效".into()); }
        let public_tls = crate::certificates::public_chain(cert)?;
        let old = self.tls.lock().unwrap().clone();
        let old_source = self.policy().cert_source;
        self.update_policy(|p|p.cert_source=source.to_owned())?;
        if let Err(error) = crate::certificates::activate(&self.config_path, cert, key).await {
            return match self.update_policy(|p|p.cert_source=old_source.clone()) {
                Ok(_) => Err(error),
                Err(restore) => Err(format!("{error}；恢复原续期模式也失败：{restore}")),
            };
        }
        *self.tls.lock().unwrap() = (cert.to_string_lossy().into_owned(), key.to_string_lossy().into_owned());
        *self.public_tls.lock().unwrap() = Some(public_tls);
        f50_server::settings::write_private(&self.state_dir.join("tls-previous.json"), &serde_json::to_vec(&json!({"cert":old.0,"key":old.1,"source":old_source})).map_err(|e|e.to_string())?)
            .map_err(|e|format!("新证书和续期模式已启用，但回滚记录保存失败：{e}"))?;
        Ok(())
    }
    /// Validate locally and reserve the single certificate job before reporting acceptance.
    fn certificate_job(self: &Arc<Self>) -> Result<(tokio::sync::OwnedSemaphorePermit, DdnsPrefs, crate::acme::Provider), String> {
        let permit = self.ddns_busy.clone().try_acquire_owned().map_err(|_| "证书任务正在进行".to_owned())?;
        let p = self.prefs().ddns;
        let request = (|| {
            let origin = reqwest::Url::parse(&self.origin).map_err(|e| e.to_string())?;
            if p.domain.is_empty() || p.domain.parse::<std::net::IpAddr>().is_ok() || !origin.host_str().is_some_and(|host| host.eq_ignore_ascii_case(&p.domain)) {
                return Err("证书域名必须与当前运行地址的域名一致；不能使用 IP 地址".to_owned());
            }
            self.provider(&p)
        })();
        let mut state = self.ddns_state.lock().unwrap();
        match request {
            Ok(provider) => {
                state.running = true; state.cert_ok = None; state.cert_result = None;
                state.cert_progress = Some("正在连接证书服务并准备 DNS-01 验证".into());
                Ok((permit, p, provider))
            }
            Err(error) => {
                state.running = false; state.cert_progress = None;
                state.cert_ms = Some(diag::now_ms()); state.cert_ok = Some(false); state.cert_result = Some(error.clone());
                Err(error)
            }
        }
    }
    pub fn start_certificate_renewal(self: &Arc<Self>) -> Result<(), String> {
        let (permit, p, provider) = self.certificate_job()?;
        let admin = self.clone();
        tokio::spawn(async move { let _ = admin.finish_certificate_job(permit, p, provider).await; });
        Ok(())
    }
    pub async fn renew_certificate(self: &Arc<Self>) -> Result<String, String> {
        let (permit, p, provider) = self.certificate_job()?;
        self.finish_certificate_job(permit, p, provider).await
    }
    async fn finish_certificate_job(self: &Arc<Self>, _permit: tokio::sync::OwnedSemaphorePermit, p: DdnsPrefs, provider: crate::acme::Provider) -> Result<String, String> {
        let state = self.clone();
        let progress = move |step: &str| {
            state.ddns_state.lock().unwrap().cert_progress = Some(step.to_owned());
            f50_server::diag_log!(Info, "web", "ACME: {step}");
        };
        let result: Result<String, String> = async {
            let issued = crate::acme::issue(&p.domain, &provider, &p.email, p.staging, &self.state_dir.join("acme-account.pem"), &progress).await?;
            let (cert, key) = crate::certificates::store_pair(&self.state_dir, &issued, p.staging)?;
            if p.staging {
                return Ok(format!("测试证书已保存至 {}；当前网页证书保持不变", cert.parent().unwrap().display()));
            }
            self.activate_certificate_pair(&cert, &key, "acme").await?;
            let cert = cert.to_string_lossy().into_owned();
            let days = tls_info(&cert).map(|(d, _)| d).unwrap_or(0);
            Ok(format!("已签发并启用 {} · 剩 {days} 天", p.domain))
        }
        .await;
        let mut s = self.ddns_state.lock().unwrap();
        s.running = false;
        s.cert_progress = None;
        s.cert_ms = Some(diag::now_ms());
        s.cert_ok = Some(result.is_ok());
        s.cert_result = Some(match &result { Ok(m) => m.clone(), Err(err) => err.clone() });
        drop(s);
        match &result {
            Ok(m) => diag().event("ok", "web", format!("证书续期：{m}")),
            Err(err) => diag().event("warn", "web", format!("证书续期失败：{err}")),
        }
        result
    }
    /// Background schedule: DDNS every interval; certificate check twice a day.
    pub async fn ddns_loop(self: Arc<Self>) {
        let mut last_update = None::<std::time::Instant>;
        let mut last_cert = None::<std::time::Instant>;
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let p = self.prefs().ddns;
            if p.enabled && last_update.is_none_or(|t| t.elapsed() >= Duration::from_secs(p.interval_min as u64 * 60)) {
                last_update = Some(std::time::Instant::now());
                let _ = self.ddns_update().await;
            }
            if p.cert && last_cert.is_none_or(|t| t.elapsed() >= Duration::from_secs(12 * 3600)) {
                last_cert = Some(std::time::Instant::now());
                if tls_info(&self.tls.lock().unwrap().0).is_ok_and(|(days, _)| days < 30) {
                    let _ = self.renew_certificate().await;
                }
            }
        }
    }
}
