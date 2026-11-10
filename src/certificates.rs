//! Persist a complete certificate pair before atomically selecting it in config.json.
use crate::acme::{Issued, Result};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::path::{Path, PathBuf};

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    f50_server::settings::write_private(path, data).map_err(|e| e.to_string())
}

pub fn store_pair(state: &Path, issued: &Issued, staging: bool) -> Result<(PathBuf, PathBuf)> {
    let certs = CertificateDer::pem_slice_iter(issued.chain_pem.as_bytes())
        .collect::<std::result::Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    if certs.is_empty() { return Err("证书链为空".into()); }
    let key = PrivateKeyDer::from_pem_slice(issued.key_pem.as_bytes()).map_err(|e| e.to_string())?;
    // Includes key/certificate matching; no existing file is touched if validation fails.
    rustls::sign::CertifiedKey::from_der(certs, key, &rustls::crypto::ring::default_provider())
        .map_err(|e| format!("证书与私钥无效或不匹配：{e}"))?;
    let dir = tempfile::Builder::new().prefix(if staging { "tls-staging-" } else { "tls-" })
        .tempdir_in(state).map_err(|e| e.to_string())?;
    let cert = dir.path().join("chain.pem");
    let key = dir.path().join("key.pem");
    write_private(&cert, issued.chain_pem.as_bytes())?;
    write_private(&key, issued.key_pem.as_bytes())?;
    // Keep the validated pair before selecting it: unwinding after config commit
    // must never remove files referenced by the committed configuration.
    let _kept = dir.keep();
    std::fs::File::open(state).and_then(|f| f.sync_all()).map_err(|e| e.to_string())?;
    Ok((cert, key))
}

/// One atomic config replacement selects both files, including across power loss.
/// Old pairs remain available for rollback; accessory identity fields are preserved.
pub fn select_pair(config: &Path, cert: &Path, key: &Path) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    let original = f50_server::settings::read(config.to_str().ok_or("配置路径不是 UTF-8")?, 32768).map_err(|e| e.to_string())?;
    let mut value: serde_json::Value = serde_json::from_slice(&original).map_err(|e| e.to_string())?;
    let web = value.get_mut("receiver").and_then(|r| r.get_mut("web"))
        .and_then(serde_json::Value::as_object_mut).ok_or("配置缺少 receiver.web")?;
    web.insert("tls_certificate".into(), cert.to_str().ok_or("证书路径不是 UTF-8")?.into());
    web.insert("tls_private_key".into(), key.to_str().ok_or("私钥路径不是 UTF-8")?.into());
    write_private(config, &serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?)?;
    Ok(original)
}

/// Commit both paths together, restoring the original selection if hot reload fails.
pub async fn activate(config: &Path, cert: &Path, key: &Path) -> Result<()> {
    let original = select_pair(config, cert, key)?;
    let cert = cert.to_str().ok_or("证书路径不是 UTF-8")?;
    let key = key.to_str().ok_or("私钥路径不是 UTF-8")?;
    if let Err(error) = f50_server::reload_tls(cert, key).await {
        write_private(config, &original)
            .map_err(|rollback| format!("热加载失败：{error}；配置恢复失败：{rollback}"))?;
        return Err(format!("证书热加载失败，已恢复原配置：{error}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> Issued {
        let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec!["cowplay.test".into()]).unwrap();
        Issued { chain_pem: cert.pem(), key_pem: signing_key.serialize_pem() }
    }
    #[test]
    fn complete_pair_is_selected_together_and_keeps_previous_files() {
        let state = tempfile::tempdir().unwrap();
        let config = state.path().join("config.json");
        let previous = br#"{"receiver":{"web":{"tls_certificate":"old-chain","tls_private_key":"old-key","access_token":"synthetic"}},"identity":"preserved"}"#;
        write_private(&config, previous).unwrap();
        let (cert, key) = store_pair(state.path(), &pair(), false).unwrap();
        assert_eq!(cert.parent(), key.parent());
        assert!(cert.is_file() && key.is_file());
        assert_eq!(&*select_pair(&config, &cert, &key).unwrap(), previous);
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(value["identity"], "preserved");
        assert_eq!(value["receiver"]["web"]["tls_certificate"], cert.to_str().unwrap());
        assert_eq!(value["receiver"]["web"]["tls_private_key"], key.to_str().unwrap());
        assert_eq!(value["receiver"]["web"]["access_token"], "synthetic");
    }
    #[test]
    fn mismatched_key_is_rejected_without_creating_candidate_files() {
        let state = tempfile::tempdir().unwrap();
        let mut issued = pair();
        issued.key_pem = pair().key_pem;
        assert!(store_pair(state.path(), &issued, false).is_err());
        assert_eq!(std::fs::read_dir(state.path()).unwrap().count(), 0);
    }
    #[test]
    fn staging_pair_does_not_select_or_overwrite_serving_files() {
        let state = tempfile::tempdir().unwrap();
        let config = state.path().join("config.json");
        write_private(&config, b"serving config").unwrap();
        let (cert, key) = store_pair(state.path(), &pair(), true).unwrap();
        assert!(cert.parent().unwrap().file_name().unwrap().to_str().unwrap().starts_with("tls-staging-"));
        assert!(key.is_file());
        assert_eq!(std::fs::read(config).unwrap(), b"serving config");
    }
    #[tokio::test]
    async fn running_tls_switches_to_the_complete_pair_and_failed_reload_restores_config() {
        let state = tempfile::tempdir().unwrap();
        let original = pair();
        let (old_cert, old_key) = store_pair(state.path(), &original, false).unwrap();
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = reserved.local_addr().unwrap();
        drop(reserved);
        let origin = format!("https://cowplay.test:{}", addr.port());
        let config = f50_server::Config { bind: addr, origin: origin.clone(), tls_certificate: old_cert.to_str().unwrap().into(), tls_private_key: old_key.to_str().unwrap().into(), access_token: "synthetic-token-for-tests-only".into() };
        let config_file = state.path().join("config.json");
        write_private(&config_file, &serde_json::to_vec(&serde_json::json!({"receiver":{"web":{"tls_certificate":old_cert,"tls_private_key":old_key,"origin":origin}},"identity":"preserved"})).unwrap()).unwrap();
        let (hub, _) = f50_server::Hub::new(config.access_token.clone(), origin.clone());
        let handle = f50_server::Handle::new();
        let shutdown = handle.clone();
        let server = tokio::spawn(f50_server::run(config, hub.clone(), handle));
        let trusted = |pem: &str| reqwest::Client::builder().no_proxy().https_only(true)
            .timeout(std::time::Duration::from_secs(2)).pool_max_idle_per_host(0)
            .add_root_certificate(reqwest::Certificate::from_pem(pem.as_bytes()).unwrap())
            .resolve("cowplay.test", addr).build().unwrap();
        let old_client = trusted(&original.chain_pem);
        let health = format!("{origin}/health");
        let started = std::time::Instant::now();
        loop {
            if old_client.get(&health).send().await.is_ok_and(|r| r.status().is_success()) { break; }
            assert!(started.elapsed() < std::time::Duration::from_secs(2), "TLS server did not start");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (manager, _bus, _, _manager_dir) = crate::bluetooth::recovery_tests::admin_fixture().await;
        let admin = crate::admin::Admin::new(manager, hub, origin.clone(), state.path().join("cowplay.uci"), config_file.clone(), state.path().to_owned(), old_cert.to_string_lossy().into_owned(), old_key.to_string_lossy().into_owned(), addr, "127.0.0.1:7000".parse().unwrap(), None, (800,480)).expect("valid synthetic policy");
        admin.update_policy(|p|p.cert_source="acme".into()).unwrap();
        let next = pair();
        let next_client = trusted(&next.chain_pem);
        let original_config = std::fs::read(&config_file).unwrap();
        let wrong_key = pair().key_pem;
        assert!(crate::ctl::dispatch(&admin, "cert_upload", &serde_json::json!({"cert":&next.chain_pem,"key":wrong_key})).await.is_err());
        let wrong_host = rcgen::generate_simple_self_signed(vec!["other.test".into()]).unwrap();
        let rejected = crate::ctl::dispatch(&admin, "cert_upload", &serde_json::json!({"cert":wrong_host.cert.pem(),"key":wrong_host.signing_key.serialize_pem()})).await.unwrap_err();
        assert!(rejected.contains("当前访问域名"));
        let expired_key = rcgen::KeyPair::generate().unwrap();
        let mut expired = rcgen::CertificateParams::new(vec!["cowplay.test".into()]).unwrap();
        expired.not_before = rcgen::date_time_ymd(2000, 1, 1);
        expired.not_after = rcgen::date_time_ymd(2001, 1, 1);
        let expired_cert = expired.self_signed(&expired_key).unwrap();
        let rejected = crate::ctl::dispatch(&admin, "cert_upload", &serde_json::json!({"cert":expired_cert.pem(),"key":expired_key.serialize_pem()})).await.unwrap_err();
        assert!(rejected.contains("尚未生效或已过期"));
        assert_eq!(std::fs::read(&config_file).unwrap(), original_config, "rejected uploads must preserve the active selection");
        assert!(old_client.get(&health).send().await.unwrap().status().is_success());
        let exported_key = format!("Bag Attributes\n    friendlyName: synthetic-only\n{}", next.key_pem);
        let uploaded = crate::ctl::dispatch(&admin, "cert_upload", &serde_json::json!({"cert":&next.chain_pem,"key":exported_key})).await.unwrap();
        assert_eq!(uploaded["ok"], true);
        assert_eq!(uploaded["names"], serde_json::json!(["cowplay.test"]));
        let (cert, key) = admin.tls.lock().unwrap().clone();
        let (cert, key) = (PathBuf::from(cert), PathBuf::from(key));
        assert_eq!(cert.parent(), key.parent());
        assert_eq!(admin.policy().cert_source,"upload");
        let previous = state.path().join("tls-previous.json");
        let previous_bytes = std::fs::read(&previous).unwrap();
        assert!(next_client.get(&health).send().await.unwrap().status().is_success());
        assert!(old_client.get(&health).send().await.is_err(), "fresh handshakes must see the new certificate");
        let selected = std::fs::read(&config_file).unwrap();
        assert!(admin.activate_certificate_pair(&cert, &old_key, "acme").await.is_err(), "mismatched key cannot become active");
        assert_eq!(admin.policy().cert_source,"upload","failed activation restores the original renewal mode");
        assert_eq!(std::fs::read(&previous).unwrap(),previous_bytes,"failed activation must not replace an existing rollback target");
        assert_eq!(std::fs::read(&config_file).unwrap(), selected, "failed reload restores the selected pair");
        assert!(next_client.get(&health).send().await.unwrap().status().is_success());
        assert!(old_cert.is_file() && old_key.is_file(), "previous files remain available for rollback");
        std::fs::remove_file(&previous).unwrap();
        std::fs::create_dir(&previous).unwrap(); // Synthetic I/O failure: a directory cannot be atomically replaced by the marker file.
        let third = pair();
        let (third_cert, third_key) = store_pair(state.path(), &third, false).unwrap();
        let error = admin.activate_certificate_pair(&third_cert, &third_key, "acme").await.unwrap_err();
        assert!(error.contains("新证书和续期模式已启用"));
        assert_eq!(admin.policy().cert_source,"acme");
        assert_eq!(*admin.tls.lock().unwrap(),(third_cert.to_string_lossy().into_owned(),third_key.to_string_lossy().into_owned()));
        assert!(trusted(&third.chain_pem).get(&health).send().await.unwrap().status().is_success());
        crate::network::verify_https(&config_file,&admin.uci_path,addr).await.unwrap();
        assert!(next_client.get(&health).send().await.is_err());
        shutdown.graceful_shutdown(Some(std::time::Duration::from_secs(1)));
        tokio::time::timeout(std::time::Duration::from_secs(2), server).await.unwrap().unwrap().unwrap();
    }

}
