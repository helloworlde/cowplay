//! Import an owner-supplied MFi pair. Never generate or export accessory credentials.
use std::path::Path;
use std::os::unix::fs::PermissionsExt;
use serde_json::Value;
use zeroize::Zeroizing;

pub(crate) fn select_pair(config: &Path, state: &Path, certificate: &str, private_key: &str) -> Result<(), String> {
    fn decode(pem: &str) -> Result<Zeroizing<Vec<u8>>, String> {
        if pem.len() > 24576 { return Err("CarPlay 文件编码后超过限制".into()); }
        let (_, bytes) = der::pem::decode_vec(pem.as_bytes()).map_err(|_|"CarPlay 文件编码无效")?;
        if bytes.is_empty() || bytes.len() > 16384 { return Err("每个 CarPlay 文件须为 1–16384 字节".into()); }
        Ok(Zeroizing::new(bytes))
    }
    let cert = decode(certificate)?;
    let key = decode(private_key)?;
    f50_core::mfi::ImportedIdentity::from_pkcs8_and_certificate(&key, &cert)
        .map_err(|_|"CarPlay 证书与私钥无效或不匹配；需要 P-256 PKCS8 DER 私钥及 X.509 DER / CMS P7B 证书")?;
    let original = f50_server::settings::read(config.to_str().ok_or("配置路径无效")?,32768).map_err(|_|"无法读取当前配置")?;
    let mut value: Value = serde_json::from_slice(&original).map_err(|_|"当前配置无效")?;
    let receiver = value.get_mut("receiver").and_then(Value::as_object_mut).ok_or("配置缺少 receiver")?;
    let dir = tempfile::Builder::new().prefix("mfi-").permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(state).map_err(|_|"无法创建 CarPlay 私有目录")?;
    let cert_path = dir.path().join("certificate.p7b");
    let key_path = dir.path().join("identity.pk8");
    f50_server::settings::write_private(&cert_path,&cert).map_err(|_|"无法保存 CarPlay 证书")?;
    f50_server::settings::write_private(&key_path,&key).map_err(|_|"无法保存 CarPlay 私钥")?;
    receiver.insert("mfi_certificate_file".into(),cert_path.to_str().ok_or("证书路径无效")?.into());
    receiver.insert("mfi_pkcs8_file".into(),key_path.to_str().ok_or("私钥路径无效")?.into());
    let bytes = Zeroizing::new(serde_json::to_vec_pretty(&value).map_err(|_|"配置编码失败")?);
    if bytes.len() > 32768 { return Err("导入后的配置超过 32 KiB，原配置保持不变".into()); }
    // Preserve a complete pair before selecting it, even if syncing config fails after rename.
    let _kept = dir.keep();
    std::fs::File::open(state).and_then(|f|f.sync_all()).map_err(|_|"同步 CarPlay 目录失败")?;
    f50_server::settings::write_private(config,&bytes).map_err(|_|"保存配置失败，请核对后重试")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> (String,String) {
        let pair = rcgen::generate_simple_self_signed(vec!["synthetic-mfi.test".into()]).unwrap();
        (pair.cert.pem(),pair.signing_key.serialize_pem())
    }
    #[test]
    fn invalid_pair_preserves_config_and_creates_no_files() {
        let dir=tempfile::tempdir().unwrap();let config=dir.path().join("config.json");
        std::fs::write(&config,b"original").unwrap();
        let (cert,_)=pair();let (_,key)=pair();
        assert!(select_pair(&config,dir.path(),&cert,&key).is_err());
        assert!(select_pair(&config,dir.path(),"invalid",&key).is_err());
        assert_eq!(std::fs::read(&config).unwrap(),b"original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(),1);
    }
    #[test]
    fn selected_pair_is_private_atomic_and_preserves_other_state() {
        use std::os::unix::fs::PermissionsExt;
        let dir=tempfile::tempdir().unwrap();let config=dir.path().join("config.json");
        std::fs::write(&config,br#"{"receiver":{"mfi_certificate_file":"old.p7b","mfi_pkcs8_file":"old.pk8","web":{"tls_certificate":"unchanged.pem","access_token":"synthetic-only"}},"state_dir":"preserved"}"#).unwrap();
        std::fs::write(dir.path().join("old.p7b"),b"old certificate").unwrap();
        std::fs::write(dir.path().join("old.pk8"),b"old key").unwrap();
        let (cert,key)=pair();select_pair(&config,dir.path(),&cert,&key).unwrap();
        let v:Value=serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(v["state_dir"],"preserved");assert_eq!(v["receiver"]["web"]["tls_certificate"],"unchanged.pem");
        assert_eq!(v["receiver"]["web"]["access_token"],"synthetic-only");
        let c=Path::new(v["receiver"]["mfi_certificate_file"].as_str().unwrap());let k=Path::new(v["receiver"]["mfi_pkcs8_file"].as_str().unwrap());
        assert_eq!(c.parent(),k.parent());assert_eq!(std::fs::metadata(k).unwrap().permissions().mode()&0o777,0o600);
        assert_eq!(std::fs::metadata(k.parent().unwrap()).unwrap().permissions().mode()&0o777,0o700);
        assert!(f50_core::mfi::ImportedIdentity::from_pkcs8_and_certificate(&std::fs::read(k).unwrap(),&std::fs::read(c).unwrap()).is_ok());
        assert_eq!(std::fs::read(dir.path().join("old.pk8")).unwrap(),b"old key");
    }
}
