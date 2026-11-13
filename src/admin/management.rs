use super::{Admin, fail, json_ok};
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path as UrlPath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc};

#[derive(Clone)]
struct Management {
    admin: Arc<Admin>,
    digest: [u8; 32],
}

pub(crate) fn load_token(token_file: Option<&Path>) -> crate::RuntimeResult<Option<[u8; 32]>> {
    let Some(path) = token_file else {
        return Ok(None);
    };
    use std::{
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        return Err(
            "management token must be an owner-only regular file owned by the service user".into(),
        );
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    file.take(129).read_to_end(&mut bytes)?;
    if bytes.len() > 128 {
        return Err("management token file too large".into());
    }
    let token = std::str::from_utf8(&bytes)?.trim();
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(
            "management token must contain 32 random bytes encoded as 64 hex characters".into(),
        );
    }
    Ok(Some(Sha256::digest(token.as_bytes()).into()))
}
pub(crate) fn router(admin: Arc<Admin>, digest: Option<[u8; 32]>) -> Router {
    let Some(digest) = digest else {
        return Router::new();
    };
    admin.management_enabled.store(true, std::sync::atomic::Ordering::Release);
    Router::new()
        .route("/api/manage/{method}", post(call))
        .layer(DefaultBodyLimit::max(65536))
        .with_state(Management { admin, digest })
}

pub(crate) fn entry(enabled: bool) -> Router {
    let target = if enabled {
        "/manage"
    } else {
        "/phone#settings"
    };
    Router::new()
        .route(
            "/admin",
            get(move || async move { Redirect::temporary(target) }),
        )
        .route(
            "/admin.html",
            get(move || async move { Redirect::temporary(target) }),
        )
}

async fn call(
    State(m): State<Management>,
    UrlPath(method): UrlPath<String>,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    // Browser cookies and the six-digit screen token do not grant maintenance access.
    if !f50_server::auth::same_origin(&m.admin.hub, &h) {
        return fail(StatusCode::FORBIDDEN, "来源不一致");
    }
    let digest: [u8; 32] = Sha256::digest(
        h.get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("")
            .as_bytes(),
    )
    .into();
    // Comparing digests does not reveal matching prefixes of the management secret.
    if digest != m.digest {
        return fail(StatusCode::UNAUTHORIZED, "管理令牌无效");
    }
    let args = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(v) => v,
        Err(_) => return fail(StatusCode::BAD_REQUEST, "请求格式无效"),
    };
    let result = match method.as_str() {
        "policy" => Ok(serde_json::json!({"policy":m.admin.policy()})),
        "policy_update" => update(&m.admin, &args),
        "tesla_status" => crate::ctl::dispatch(&m.admin, &method, &args).await,
        #[cfg(feature = "tesla")]
        "tesla_configure" | "tesla_connect" | "tesla_disconnect" | "tesla_pair" | "tesla_cancel" | "tesla_climate_pair" | "tesla_climate_query" | "tesla_climate_command" | "tesla_ultra_control" => crate::ctl::dispatch(&m.admin, &method, &args).await,
        "status" | "diag" | "screens" | "client_remove" | "token" | "token_set" | "token_disable" | "cert" | "cert_upload"
        | "mfi_upload" | "logs" => crate::ctl::dispatch(&m.admin, &method, &args).await,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    match result {
        Ok(v) => json_ok(v),
        Err(e) => fail(StatusCode::CONFLICT, e),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayPolicy {
    max_res: String,
    max_fps: u8,
    auto_downgrade: bool,
}
fn update(a: &Admin, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let p: DisplayPolicy = serde_json::from_value(args.clone()).map_err(|e| e.to_string())?;
    if crate::policy::resolution_box(&p.max_res).is_none()
        || !f50_core::info::FRAME_RATES.contains(&p.max_fps)
    {
        return Err("分辨率或帧率超出接收端范围".into());
    }
    let policy = a.update_policy(|policy| {
        policy.max_res = p.max_res;
        policy.max_fps = p.max_fps;
        policy.auto_downgrade = p.auto_downgrade;
    })?;
    a.apply_display(true);
    Ok(serde_json::json!({"policy":policy}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Store;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[tokio::test]
    async fn linux_policy_survives_restart_and_maintenance_requires_its_own_secret_and_origin() {
        let (manager, _bluez, hub, dir) = crate::bluetooth::recovery_tests::admin_fixture().await;
        let policy_path = dir.path().join("policy.json");
        let make = || {
            Admin::with_store(
                manager.clone(),
                hub.clone(),
                "https://test.invalid".into(),
                Store::Json {
                    path: policy_path.clone(),
                },
                dir.path().join("config.json"),
                dir.path().to_owned(),
                "cert".into(),
                "key".into(),
                "127.0.0.1:19001".parse().unwrap(),
                "127.0.0.1:7000".parse().unwrap(),
                None,
                (800, 480),
                false,
            )
            .unwrap()
        };
        let a = make();
        assert_eq!(
            a.policy().port,
            19001,
            "first JSON start preserves the configured HTTPS port"
        );
        assert_eq!(crate::console::network(&a)["platform"], "linux");
        assert_eq!(
            crate::console::network(&a)["luci"],
            "https://test.invalid/admin"
        );
        let token = "ab".repeat(32);
        let file = dir.path().join("management-token");
        f50_server::settings::write_private(&file, token.as_bytes()).unwrap();
        let app = router(a.clone(), load_token(Some(&file)).unwrap()).merge(entry(true));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(axum::serve(listener, app).into_future());
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let url = format!("http://{addr}/api/manage/policy_update");
        let args = serde_json::json!({"max_res":"1280x800","max_fps":30,"auto_downgrade":false});
        let browser = hub
            .clients
            .admit_token(&hub.clients.token(), "phone", "phone", "", "")
            .unwrap();
        let before = std::fs::read(&policy_path).unwrap();
        let cookie = format!("{}={}", f50_server::clients::COOKIE, browser.id);
        let r = client
            .post(&url)
            .header("Origin", "https://test.invalid")
            .header("Cookie", cookie)
            .header("Content-Type", "application/json")
            .body(args.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let change_url=format!("http://{addr}/api/manage/token_set");
        let change=serde_json::json!({"token":"345678"});
        let r=client.post(&change_url).header("Origin","https://test.invalid").header("Cookie",format!("{}={}",f50_server::clients::COOKIE,browser.id)).header("Content-Type","application/json").body(change.to_string()).send().await.unwrap();
        assert_eq!(r.status(),StatusCode::UNAUTHORIZED);
        let r=client.post(&change_url).bearer_auth(&token).header("Origin","https://other.invalid").header("Content-Type","application/json").body(change.to_string()).send().await.unwrap();
        assert_eq!(r.status(),StatusCode::FORBIDDEN);
        let r=client.post(&change_url).bearer_auth(&token).header("Origin","https://test.invalid").header("Content-Type","application/json").body(change.to_string()).send().await.unwrap();
        assert_eq!(r.status(),StatusCode::OK);assert_eq!(hub.clients.token(),"345678");assert!(hub.clients.get(&browser.id).is_some());
        let disable_url=format!("http://{addr}/api/manage/token_disable");
        let r=client.post(&disable_url).header("Origin","https://test.invalid").header("Cookie",format!("{}={}",f50_server::clients::COOKIE,browser.id)).body("{}").send().await.unwrap();
        assert_eq!(r.status(),StatusCode::UNAUTHORIZED);
        let r=client.post(&disable_url).bearer_auth(&token).header("Origin","https://other.invalid").body("{}").send().await.unwrap();assert_eq!(r.status(),StatusCode::FORBIDDEN);
        let r=client.post(&disable_url).bearer_auth(&token).header("Origin","https://test.invalid").body("{}").send().await.unwrap();assert_eq!(r.status(),StatusCode::OK);assert!(!hub.clients.policy().required);
        #[cfg(feature = "tesla")]
        {
            for method in ["tesla_configure", "tesla_connect", "tesla_disconnect", "tesla_pair", "tesla_cancel", "tesla_climate_pair", "tesla_climate_query", "tesla_climate_command", "tesla_ultra_control"] {
                let url = format!("http://{addr}/api/manage/{method}");
                let r = client.post(&url).header("Origin", "https://test.invalid")
                    .header("Cookie", format!("{}={}", f50_server::clients::COOKIE, browser.id))
                    .body("{}").send().await.unwrap();
                assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "screen cookies cannot call {method}");
                let r = client.post(&url).bearer_auth(&token).header("Origin", "https://other.invalid").body("{}").send().await.unwrap();
                assert_eq!(r.status(), StatusCode::FORBIDDEN, "maintenance still checks Origin for {method}");
            }
            let r = client.post(format!("http://{addr}/api/manage/tesla_status")).bearer_auth(&token)
                .header("Origin", "https://test.invalid").body("{}").send().await.unwrap();
            assert_eq!(r.status(), StatusCode::OK);
            let r = client.post(format!("http://{addr}/api/manage/tesla_configure")).bearer_auth(&token)
                .header("Origin", "https://test.invalid").body(r#"{"vin":"5YJ30123456789ABC","enabled":true,"private_key":"/synthetic/not-accepted.pk8"}"#).send().await.unwrap();
            assert_eq!(r.status(), StatusCode::CONFLICT, "the HTTP API never accepts an identity path");
            assert!(!dir.path().join("tesla").exists());
        }
        for origin in [None, Some("https://other.invalid")] {
            let mut req = client
                .post(&url)
                .bearer_auth(&token)
                .header("Content-Type", "application/json")
                .body(args.to_string());
            if let Some(origin) = origin {
                req = req.header("Origin", origin);
            }
            assert_eq!(req.send().await.unwrap().status(), StatusCode::FORBIDDEN);
        }
        assert_eq!(std::fs::read(&policy_path).unwrap(), before);
        let r = client
            .post(&url)
            .bearer_auth(&token)
            .header("Origin", "https://test.invalid")
            .header("Content-Type", "application/json")
            .body(args.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(make().policy().max_box(), (1280, 800));
        assert_eq!(make().policy().port, 19001);
        assert_eq!(
            std::fs::metadata(&policy_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let r = client
            .post(format!("http://{addr}/api/manage/factory_reset"))
            .bearer_auth(&token)
            .header("Origin", "https://test.invalid")
            .header("Content-Type", "application/json")
            .body(args.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = client
            .get(format!("http://{addr}/admin"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.headers()["location"], "/manage");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_token(Some(&file)).is_err());
        let link = dir.path().join("linked-token");
        symlink(&file, &link).unwrap();
        assert!(load_token(Some(&link)).is_err());
        task.abort();
    }
    #[tokio::test]
    async fn linux_uci_first_start_preserves_port_and_existing_unknown_options() {
        let (manager, _bluez, hub, dir) = crate::bluetooth::recovery_tests::admin_fixture().await;
        let path = dir.path().join("cowplay.uci");
        let make = || {
            Admin::with_store(
                manager.clone(),
                hub.clone(),
                "https://test.invalid:19001".into(),
                Store::Uci { path: path.clone() },
                dir.path().join("config.json"),
                dir.path().to_owned(),
                "cert".into(),
                "key".into(),
                "127.0.0.1:19001".parse().unwrap(),
                "127.0.0.1:7000".parse().unwrap(),
                None,
                (800, 480),
                false,
            )
            .unwrap()
        };
        assert_eq!(make().policy().port, 19001);
        let mut pkg = crate::uci::Package::read(&path).unwrap();
        pkg.set("cowplay", "main", "unknown_option", "preserve");
        pkg.write(&path).unwrap();
        let restarted = make();
        assert_eq!(restarted.policy().port, 19001);
        restarted
            .update_policy(|p| p.auto_downgrade = true)
            .unwrap();
        assert_eq!(
            crate::uci::Package::read(&path)
                .unwrap()
                .get("main", "unknown_option"),
            Some("preserve")
        );
        assert!(!path.with_extension("json").exists());
    }
}
