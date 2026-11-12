//! Local-only import of the owner's existing DashPlay material. Never generates accessory identity.
use crate::board;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::{Command, Stdio},
};
use zeroize::Zeroizing;
fn bytes(path: &Path, max: usize) -> crate::RuntimeResult<Zeroizing<Vec<u8>>> {
    let mut data = Zeroizing::new(Vec::new());
    fs::File::open(path)?
        .take((max + 1) as u64)
        .read_to_end(&mut data)?;
    if data.len() > max {
        return Err("import source too large".into());
    }
    Ok(data)
}
fn write(path: &Path, data: &[u8]) -> crate::RuntimeResult<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    f.sync_all()?;
    Ok(())
}
fn unescape(text: &str) -> crate::RuntimeResult<String> {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let c = chars.next().ok_or("unsupported property escape")?;
            match c {
                ':' | '=' | '\\' | ' ' => out.push(c),
                _ => return Err("unsupported property escape".into()),
            }
        } else {
            out.push(c)
        }
    }
    Ok(out)
}
fn properties(text: &str) -> crate::RuntimeResult<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or("malformed properties")?;
        if map.insert(unescape(key)?, unescape(value)?).is_some() {
            return Err("duplicate property".into());
        }
    }
    Ok(map)
}
fn hex32(text: &str) -> crate::RuntimeResult<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return Err("expected 32-byte hex key".into());
    }
    let mut result = [0; 32];
    for (i, b) in result.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(|_| "invalid hex key")?;
    }
    Ok(result)
}
fn command_json(command: &str, args: &[&str]) -> crate::RuntimeResult<Value> {
    let output = Command::new(command)
        .args(args)
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() || output.stdout.len() > 1024 * 1024 {
        return Err("OpenWrt network query failed".into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}
fn copy_tree(from: &Path, to: &Path, depth: usize) -> crate::RuntimeResult<()> {
    if depth > 4 {
        return Err("unexpected state directory depth".into());
    }
    board::private_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = to.join(entry.file_name());
        if ty.is_dir() {
            copy_tree(&entry.path(), &target, depth + 1)?
        } else if ty.is_file() {
            write(&target, &bytes(&entry.path(), 1024 * 1024)?)?
        } else {
            return Err("state contains an unsupported file type (symlink or device)".into());
        }
    }
    Ok(())
}
fn text<'a>(value: &'a Value, key: &str) -> crate::RuntimeResult<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing imported field: {key}").into())
}
pub fn from_dashplay(source: &Path, destination: &Path) -> crate::RuntimeResult<()> {
    let source_root = fs::canonicalize(source)?;
    if !destination.is_absolute()
        || destination
            .components()
            .any(|p| matches!(p, std::path::Component::ParentDir))
    {
        return Err("absolute destination without parent components required".into());
    }
    let mut ancestor = destination;
    while fs::symlink_metadata(ancestor).is_err() {
        ancestor = ancestor
            .parent()
            .ok_or("destination has no existing ancestor")?;
    }
    let resolved = fs::canonicalize(ancestor)?.join(destination.strip_prefix(ancestor)?);
    if resolved.starts_with(&source_root) {
        return Err("destination must be outside the read-only source tree".into());
    }

    if fs::symlink_metadata(destination).is_ok() {
        return Err("destination already exists; existing data preserved".into());
    }
    let receiver = source.join("state/receiver");
    let old: Value = serde_json::from_slice(&bytes(&receiver.join("config.json"), 16384)?)?;
    let identity = bytes(&receiver.join("airplay-identity.properties"), 16384)?;
    let identity = properties(std::str::from_utf8(&identity)?)?;
    let seed = Zeroizing::new(hex32(
        identity.get("privateKey").ok_or("missing signing seed")?,
    )?);
    let public = hex32(identity.get("publicKey").ok_or("missing public key")?)?;
    if f50_core::pairing::public_key(&seed) != public {
        return Err("stored AirPlay signing key mismatch".into());
    }
    let pairing_id = identity.get("pairingId").ok_or("missing pairing ID")?;
    let address = fs::read_to_string(source.join("state/bdaddr"))?;
    if !board::valid_address(&address) {
        return Err("existing Bluetooth address is invalid".into());
    }
    let network = command_json("ubus", &["call", "network.wireless", "status"])?;
    let desired = text(&old, "wifiSsid")?;
    let mut matching = Vec::new();
    for radio in network
        .as_object()
        .ok_or("unexpected wireless status")?
        .values()
    {
        if radio.get("up").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        if let Some(interfaces) = radio.get("interfaces").and_then(Value::as_array) {
            for interface in interfaces {
                let cfg = &interface["config"];
                if cfg["mode"] == "ap" && cfg["ssid"].as_str() == Some(desired) {
                    matching.push((radio, cfg));
                }
            }
        }
    }
    if matching.len() != 1 {
        return Err("existing configured SSID does not identify one active access point".into());
    }
    let (radio, wifi) = matching[0];
    let encryption = text(wifi, "encryption")?;
    let security =
        crate::handoff_security_code(encryption).ok_or("unsupported actual Wi-Fi security")?;
    let channel = radio["config"]["channel"]
        .as_str()
        .and_then(|s| s.parse::<u8>().ok())
        .or_else(|| {
            radio["config"]["channel"]
                .as_u64()
                .and_then(|n| u8::try_from(n).ok())
        })
        .ok_or("actual Wi-Fi channel unavailable")?;
    let password = if security == 0 {
        ""
    } else {
        text(wifi, "key")?
    };
    let lan = command_json("ubus", &["call", "network.interface.lan", "status"])?;
    let address_ip = lan["ipv4-address"]
        .as_array()
        .and_then(|v| v.first())
        .and_then(|v| v["address"].as_str())
        .ok_or("LAN IPv4 unavailable")?;
    let certificate = source.join("state/tls/own/fullchain.pem");
    let private_key = source.join("state/tls/own/key.pem");
    let certificate_text = Command::new("openssl")
        .args(["x509", "-noout", "-ext", "subjectAltName", "-in"])
        .arg(&certificate)
        .stderr(Stdio::null())
        .output()?;
    if !certificate_text.status.success() {
        return Err("existing trusted TLS certificate unavailable".into());
    }
    let names = String::from_utf8(certificate_text.stdout)?;
    let domain = names
        .split([',', '\n'])
        .map(str::trim)
        .filter_map(|s| s.strip_prefix("DNS:"))
        .find(|s| !s.contains('*'))
        .ok_or("existing TLS certificate has no concrete DNS name")?;
    let digest = Sha256::digest(public);
    let device_id = format!(
        "02:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        digest[0], digest[1], digest[2], digest[3], digest[4]
    );
    let pairs = match bytes(&receiver.join("pairings.properties"), 16384) {
        Ok(data) => properties(std::str::from_utf8(&data)?)?,
        Err(_) => BTreeMap::new(),
    };
    if pairs.len() > 1 {
        return Err(
            "multiple saved AirPlay controllers require explicit selection; source preserved"
                .into(),
        );
    }
    let parent = destination.parent().ok_or("destination parent required")?;
    fs::create_dir_all(parent)?;
    let stage = parent.join(format!(".carplay-import-{}", std::process::id()));
    if stage.exists() {
        return Err("an interrupted import candidate exists; preserved for inspection".into());
    }
    board::private_dir(&stage)?;
    let result = (|| {
        write(&stage.join("signing-seed"), seed.as_slice())?;
        write(&stage.join("bdaddr"), address.as_bytes())?;
        write(&stage.join("wifi-password"), password.as_bytes())?;
        write(
            &stage.join("accessory.pk8"),
            &bytes(&source.join("identity/offline-mfi/identity.pk8"), 16384)?,
        )?;
        write(
            &stage.join("accessory.p7b"),
            &bytes(&source.join("identity/offline-mfi/certificate.p7b"), 16384)?,
        )?;
        write(&stage.join("tls.pem"), &bytes(&certificate, 32768)?)?;
        write(&stage.join("tls.key"), &bytes(&private_key, 16384)?)?;
        let token = f50_server::clients::random_token()?;
        write(&stage.join("access-token"), token.as_bytes())?;
        copy_tree(&source.join("state/bluetooth"), &stage.join("bluetooth"), 0)?;
        let mut phone = old["bluetoothDeviceAddress"].as_str().map(str::to_owned);
        if phone.is_none() {
            let adapter = source
                .join("state/bluetooth")
                .join(address.trim().to_ascii_uppercase());
            if let Ok(entries) = fs::read_dir(adapter) {
                let phones: Vec<_> = entries
                    .filter_map(Result::ok)
                    .filter(|e| e.path().join("info").is_file())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|s| board::valid_address(s))
                    .collect();
                if phones.len() == 1 {
                    phone = phones.into_iter().next();
                }
            }
        }
        if let Some(phone) = phone {
            if !board::valid_address(&phone) {
                return Err("stored phone address is invalid".into());
            }
            let path = format!(
                "/org/bluez/hci0/dev_{}",
                phone.to_ascii_uppercase().replace(':', "_")
            );
            write(&stage.join("phone"), path.as_bytes())?;
        }

        if let Some((id, key)) = pairs.iter().next() {
            f50_server::pairing_store::write(
                &stage.join("pairing.json"),
                id.as_bytes(),
                &hex32(key)?,
            )?;
        }
        let path = |file: &str| destination.join(file);
        let config = json!({"state_dir":destination,"bluetooth_address_file":path("bdaddr"),"receiver":{
            "web":{"bind":format!("{address_ip}:18443"),"origin":format!("https://{domain}:18443"),"tls_certificate":path("tls.pem"),"tls_private_key":path("tls.key"),"access_token":token},
            "control_bind":format!("{address_ip}:7000"),"pairing_id":pairing_id,"pairing_store_file":path("pairing.json"),"signing_seed_file":path("signing-seed"),"mfi_pkcs8_file":path("accessory.pk8"),"mfi_certificate_file":path("accessory.p7b"),"controller_id":null,"controller_key_file":null,
            "name":"CowPlay","model":"DashPlay","manufacturer":"DashPlay","device_id":device_id,"bluetooth_mac":address.trim(),"width":1280,"height":720,"pairing_window_seconds":120,
            "wifi":{"ssid":desired,"password_file":path("wifi-password"),"channel":channel,"security":security,"serial":"cowplay"}}});
        // Validate imported cryptographic material locally before publishing the destination.
        let mut staged = config["receiver"].clone();
        for field in [
            "signing_seed_file",
            "mfi_pkcs8_file",
            "mfi_certificate_file",
            "controller_key_file",
            "pairing_store_file",
        ] {
            if let Some(value) = staged[field].as_str() {
                let name = Path::new(value)
                    .file_name()
                    .ok_or("missing staged file name")?;
                staged[field] = json!(stage.join(name));
            }
        }
        staged["web"]["tls_certificate"] = json!(stage.join("tls.pem"));
        staged["web"]["tls_private_key"] = json!(stage.join("tls.key"));
        staged["wifi"]["password_file"] = json!(stage.join("wifi-password"));
        serde_json::from_value::<f50_server::settings::Settings>(staged)?.prepare()?;
        write(
            &stage.join("config.json"),
            &serde_json::to_vec_pretty(&config)?,
        )?;
        fs::rename(&stage, destination)?;
        println!("IMPORTED_EXISTING_MATERIAL_LOCALLY; source unchanged; secrets not displayed");
        Ok(())
    })();
    // On failure preserve the private candidate for diagnosis; never alter original material.
    result
}
/// Product name announced to the iPhone and in Bluetooth after the CowPlay rename.
pub const PRODUCT_NAME: &str = "CowPlay";
const LEGACY_NAMES: [&str; 2] = ["CarPlay OpenWrt", "DashPlay"];
fn rebase(value: &mut Value, old: &str, new: &str) {
    match value {
        Value::String(s) => {
            if let Some(rest) = s.strip_prefix(old) {
                if rest.is_empty() || rest.starts_with('/') {
                    *s = format!("{new}{rest}");
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| rebase(v, old, new)),
        Value::Object(map) => map.values_mut().for_each(|v| rebase(v, old, new)),
        _ => {}
    }
}
fn rename_alias(settings: &str) -> Option<String> {
    let mut changed = false;
    let out: Vec<String> = settings
        .lines()
        .map(|line| match line.strip_prefix("Alias=") {
            Some(name) if LEGACY_NAMES.contains(&name.trim()) => {
                changed = true;
                format!("Alias={PRODUCT_NAME}")
            }
            _ => line.to_owned(),
        })
        .collect();
    changed.then(|| out.join("\n") + if settings.ends_with('\n') { "\n" } else { "" })
}
/// One-time copy of the pre-rename state directory. The source is never modified; the
/// destination appears atomically and only after config paths point at it.
pub fn migrate_state(old: &Path, new: &Path) -> crate::RuntimeResult<()> {
    if !old.is_absolute() || !new.is_absolute() {
        return Err("state paths must be absolute".into());
    }
    if fs::symlink_metadata(new).is_ok() {
        return Err("new state directory already exists; nothing migrated".into());
    }
    let source = fs::canonicalize(old)?;
    let parent = new.parent().ok_or("new state directory needs a parent")?;
    fs::create_dir_all(parent)?;
    let stage = parent.join(format!(
        ".{}.migrating",
        new.file_name().ok_or("new state directory needs a name")?.to_string_lossy()
    ));
    if fs::symlink_metadata(&stage).is_ok() {
        return Err("an earlier migration left a staging directory; inspect and remove it first".into());
    }
    copy_tree(&source, &stage, 0)?;
    let config_path = stage.join("config.json");
    let mut config: Value = serde_json::from_slice(&bytes(&config_path, 64 * 1024)?)?;
    let (from, to) = (old.to_string_lossy().into_owned(), new.to_string_lossy().into_owned());
    rebase(&mut config, &from, &to);
    if let Some(name) = config.pointer_mut("/receiver/name") {
        if name.as_str().is_some_and(|n| LEGACY_NAMES.contains(&n)) {
            *name = json!(PRODUCT_NAME);
        }
    }
    fs::remove_file(&config_path)?;
    write(&config_path, &serde_json::to_vec_pretty(&config)?)?;
    if let Ok(adapters) = fs::read_dir(stage.join("bluetooth")) {
        for adapter in adapters {
            let settings = adapter?.path().join("settings");
            let Ok(text) = fs::read_to_string(&settings) else { continue };
            if let Some(updated) = rename_alias(&text) {
                fs::remove_file(&settings)?;
                write(&settings, updated.as_bytes())?;
            }
        }
    }
    fs::rename(&stage, new)?;
    println!("STATE_MIGRATED {} -> {}; source unchanged", from, to);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_rebases_paths_renames_device_and_keeps_source() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("cowplay-migrate-{}-{nonce}", std::process::id()));
        let old = root.join("carplay-openwrt/state");
        let new = root.join("cowplay/state");
        board::private_dir(&old.join("bluetooth/AA:BB:CC:DD:EE:FF")).unwrap();
        let o = old.to_string_lossy();
        let config = json!({"state_dir": o, "bluetooth_address_file": format!("{o}/bdaddr"),
            "receiver": {"name": "CarPlay OpenWrt", "web": {"tls_certificate": format!("{o}/tls.pem"), "origin": "https://x:18443"},
            "other": format!("{o}-sibling/x"), "model": "DashPlay"}});
        fs::write(old.join("config.json"), serde_json::to_vec(&config).unwrap()).unwrap();
        fs::write(old.join("bluetooth/AA:BB:CC:DD:EE:FF/settings"), "[General]\nAlias=DashPlay\nDiscoverable=false\n").unwrap();
        fs::write(old.join("signing-seed"), [7u8; 32]).unwrap();
        migrate_state(&old, &new).unwrap();
        let moved: Value = serde_json::from_slice(&fs::read(new.join("config.json")).unwrap()).unwrap();
        let n = new.to_string_lossy();
        assert_eq!(moved["state_dir"], json!(n));
        assert_eq!(moved["receiver"]["web"]["tls_certificate"], json!(format!("{n}/tls.pem")));
        assert_eq!(moved["receiver"]["other"], json!(format!("{o}-sibling/x")), "only whole path prefixes move");
        assert_eq!(moved["receiver"]["name"], json!("CowPlay"));
        assert_eq!(moved["receiver"]["model"], json!("DashPlay"), "accessory identity fields stay");
        assert_eq!(fs::read_to_string(new.join("bluetooth/AA:BB:CC:DD:EE:FF/settings")).unwrap(), "[General]\nAlias=CowPlay\nDiscoverable=false\n");
        assert_eq!(fs::read(new.join("signing-seed")).unwrap(), vec![7u8; 32]);
        assert_eq!(fs::read(old.join("config.json")).unwrap(), serde_json::to_vec(&config).unwrap(), "source unchanged");
        assert!(migrate_state(&old, &new).is_err(), "never overwrites an existing destination");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn known_java_properties_preserve_escaped_identifier() {
        let p = properties("# test\nid\\:one=abcd\nprivateKey=0000\n").unwrap();
        assert_eq!(p["id:one"], "abcd");
        assert!(properties("x=1\nx=2").is_err());
        assert!(properties("x=foo\\u1234").is_err());
    }
    #[test]
    fn malformed_hex_never_panics() {
        for value in ["short", &"é".repeat(32), &"z".repeat(64)] {
            assert!(hex32(value).is_err());
        }
        assert_eq!(hex32(&"ab".repeat(32)).unwrap(), [0xab; 32]);
    }
}
