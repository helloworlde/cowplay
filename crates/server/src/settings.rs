use crate::{
    receiver::{Credentials, Pairings},
    Config,
};
use f50_core::{
    info::{self, DisplayInfo},
    mfi::ImportedIdentity,
    wireless::Endpoint,
};
use std::{
    net::SocketAddr,
    path::Path,
    sync::Arc,
    time::Duration,
};
use zeroize::Zeroizing;
type Failure = Box<dyn std::error::Error + Send + Sync>;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub web: Config,
    pub control_bind: SocketAddr,
    pub pairing_id: String,
    pub pairing_store_file: Option<String>,
    pub signing_seed_file: String,
    pub mfi_pkcs8_file: String,
    pub mfi_certificate_file: String,
    pub controller_id: Option<String>,
    pub controller_key_file: Option<String>,
    pub name: String,
    pub model: String,
    pub manufacturer: String,
    pub device_id: String,
    pub bluetooth_mac: String,
    pub width: u16,
    pub height: u16,
    pub pairing_window_seconds: u64,
    pub wifi: Option<Wifi>,
}
pub fn read(path: &str, max: usize) -> Result<Zeroizing<Vec<u8>>, Failure> {
    use std::io::Read;
    let mut b = Zeroizing::new(Vec::new());
    std::fs::File::open(path)?
        .take((max + 1) as u64)
        .read_to_end(&mut b)?;
    if b.len() > max {
        return Err("input size limit".into());
    }
    Ok(b)
}
/// Per-process sequence for temporary names in [`replace`].
static NEXT_TEMPORARY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Atomically replace `path` with `data`, created with `mode`. A uniquely named temporary file
/// in the same directory is synced, renamed over `path`, then the directory is synced, so a
/// power loss leaves either the old or the new content. The temporary is removed on failure.
pub fn replace(path: &Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt, sync::atomic::Ordering};
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().ok_or_else(|| std::io::Error::other("file name required"))?;
    let mut collisions = 0;
    let (temporary, mut file) = loop {
        let temporary = parent.join(format!(
            ".{}.{}.{}.tmp",
            name.to_string_lossy(),
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&temporary) {
            Ok(file) => break (temporary, file),
            // Left by an earlier boot that had the same pid and lost power mid-write; never ours.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && collisions < 64 => collisions += 1,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        file.write_all(data)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        std::fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
/// [`replace`] for state readable only by the service (pairings, tokens, keys, selections).
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    replace(path, data, 0o600)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wifi {
    pub ssid: String,
    pub password_file: String,
    pub channel: u8,
    pub security: u8,
    pub serial: String,
}
pub struct Prepared {
    pub web: Config,
    pub control_bind: SocketAddr,
    pub credentials: Arc<Credentials>,
    pub pairings: Pairings,
    pub pairing_window: Duration,
}
impl Settings {
    pub fn prepare(self) -> Result<Prepared, Failure> {
        let cfg = self;
        cfg.web.validate()?;
        if cfg.control_bind.ip().is_unspecified()
            || cfg.control_bind.ip().is_multicast()
            || cfg.control_bind.port() == 0
            || cfg.pairing_window_seconds > 120
            || cfg.pairing_id.is_empty()
            || cfg.pairing_id.len() > 128
        {
            return Err("explicit local address, pairing ID and window required".into());
        }
        if cfg.mfi_pkcs8_file == cfg.web.tls_private_key
            || cfg.signing_seed_file == cfg.web.tls_private_key
        {
            return Err("accessory and web TLS identities must be separate".into());
        }
        let seed = read(&cfg.signing_seed_file, 32)?;
        let signing_seed = Zeroizing::new(
            <[u8; 32]>::try_from(seed.as_slice())
                .map_err(|_| "signing seed must be exactly 32 bytes")?,
        );
        let private = read(&cfg.mfi_pkcs8_file, 16384)?;
        let certificate = read(&cfg.mfi_certificate_file, 16384)?;
        let mfi =
            ImportedIdentity::from_pkcs8_and_certificate(&private, &certificate).map_err(|_| {
                "MFi identity import failed; requires matching P256 PKCS8 DER and certificate/CMS"
            })?;
        let mut pairings = match (&cfg.controller_id, &cfg.controller_key_file) {
            (None, None) => None,
            (Some(id), Some(path)) if !id.is_empty() && id.len() <= 128 => {
                let bytes = read(path, 32)?;
                Some((
                    id.as_bytes().to_vec(),
                    <[u8; 32]>::try_from(bytes.as_slice())
                        .map_err(|_| "controller public key must be 32 bytes")?,
                ))
            }
            _ => return Err("controller ID and public key must be provided together".into()),
        };
        if pairings.is_none() {
            if let Some(path) = &cfg.pairing_store_file {
                pairings = crate::pairing_store::read(std::path::Path::new(path))?;
            }
        }
        let info = info::build(&DisplayInfo {
            name: &cfg.name,
            model: &cfg.model,
            manufacturer: &cfg.manufacturer,
            device_id: &cfg.device_id,
            bluetooth_mac: &cfg.bluetooth_mac,
            width: cfg.width,
            height: cfg.height,
            fps: 30,
            width_mm: 180,
            hevc: false,
            microphone: false, right_hand_drive: false,
        })
        .map_err(|_| "invalid info configuration")?;
        let info_base = crate::receiver::InfoBase {
            name: cfg.name.clone(),
            model: cfg.model.clone(),
            manufacturer: cfg.manufacturer.clone(),
            device_id: cfg.device_id.clone(),
            bluetooth_mac: cfg.bluetooth_mac.clone(),
        };

        let endpoint = if let Some(wifi) = cfg.wifi {
            let octets = cfg
                .bluetooth_mac
                .split(':')
                .map(|s| u8::from_str_radix(s, 16))
                .collect::<Result<Vec<_>, _>>()?;
            let mac: [u8; 6] = octets
                .try_into()
                .map_err(|_| "Bluetooth MAC requires six bytes")?;
            let password = read(&wifi.password_file, 63)?;
            let password = std::str::from_utf8(&password)?.to_owned();
            let e = Endpoint {
                name: cfg.name,
                model: cfg.model,
                manufacturer: cfg.manufacturer,
                serial: wifi.serial,
                bluetooth_mac: mac,
                ssid: wifi.ssid,
                password: Zeroizing::new(password),
                channel: wifi.channel,
                security: wifi.security,
                addresses: vec![cfg.control_bind.ip()],
                airplay_port: cfg.control_bind.port(),
                pairing_public_key: f50_core::pairing::public_key(&signing_seed),
                source_version: "366.0".into(),
            };
            e.validate().map_err(|_| "invalid wireless endpoint")?;
            Some(Arc::new(e))
        } else {
            None
        };
        Ok(Prepared {
            web: cfg.web,
            control_bind: cfg.control_bind,
            credentials: Arc::new(Credentials {
                pairing_store: cfg.pairing_store_file.clone().map(Into::into),
                pairing_id: cfg.pairing_id.into_bytes(),
                signing_seed,
                mfi: Arc::new(mfi),
                info,
                info_base,
                width: cfg.width,
                height: cfg.height,
                endpoint,
            }),
            pairings: crate::phones::Phones::new(pairings, cfg.pairing_store_file.clone().map(Into::into)),
            pairing_window: Duration::from_secs(cfg.pairing_window_seconds),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_is_private_atomic_and_cleans_up_failed_writes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("f50-replace-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("phone");
        write_private(&path, b"old").unwrap();
        write_private(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        replace(&dir.join("shared"), b"uci", 0o644).unwrap();
        assert_eq!(std::fs::read(dir.join("shared")).unwrap(), b"uci");
        let occupied = dir.join("occupied");
        std::fs::create_dir(&occupied).unwrap();
        assert!(write_private(&occupied, b"cannot replace a directory").is_err());
        let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        assert_eq!(names, ["occupied", "phone", "shared"], "no temporary file is left behind");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn temporaries_left_by_a_power_loss_with_the_same_pid_do_not_block_writes() {
        let dir = std::env::temp_dir().join(format!("f50-replace-leftover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let next = NEXT_TEMPORARY.load(std::sync::atomic::Ordering::Relaxed);
        let leftovers: Vec<_> = (next..next + 8)
            .map(|n| dir.join(format!(".record.{}.{n}.tmp", std::process::id())))
            .collect();
        for leftover in &leftovers {
            std::fs::write(leftover, b"truncated").unwrap();
        }
        write_private(&dir.join("record"), b"complete").unwrap();
        assert_eq!(std::fs::read(dir.join("record")).unwrap(), b"complete");
        for leftover in &leftovers {
            assert_eq!(std::fs::read(leftover).unwrap(), b"truncated", "a file this call did not create is untouched");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
