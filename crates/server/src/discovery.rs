//! Explicit-interface AirPlay registration. Never browses peers or changes network settings.
use crate::receiver::Credentials;
use mdns_sd::{DaemonEvent, IfKind, ServiceDaemon, ServiceInfo};
use std::{io, net::SocketAddr};
fn error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
pub struct Discovery {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<DaemonEvent>,
    fullname: String,
    address: std::net::IpAddr,
}
fn service(c: &Credentials, address: SocketAddr) -> io::Result<ServiceInfo> {
    let e = c
        .endpoint
        .as_ref()
        .ok_or_else(|| error("wireless endpoint required"))?;
    let info = f50_core::bplist::decode(&c.info).map_err(|_| error("invalid info"))?;
    let dictionary = info
        .as_dictionary()
        .ok_or_else(|| error("invalid info dictionary"))?;
    let capabilities = dictionary
        .get("features")
        .and_then(f50_core::bplist::Value::as_unsigned_integer)
        .ok_or_else(|| error("info features missing"))?;
    // The discovery TXT and RTSP /info describe the same receiver, including audio.
    let low = capabilities as u32;
    let high = capabilities >> 32;
    let features = if high == 0 {
        format!("0x{low:x}")
    } else {
        format!("0x{low:x},0x{high:x}")
    };
    let device = dictionary
        .get("deviceID")
        .and_then(f50_core::bplist::Value::as_string)
        .ok_or_else(|| error("device ID missing"))?;
    let key = e
        .pairing_public_key
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let pairing = std::str::from_utf8(&c.pairing_id).map_err(error)?;
    let hostname = format!("cowplay-{}.local.", &key[..12]);
    let txt = [
        ("deviceid", device),
        ("features", features.as_str()),
        ("flags", "0x4"),
        ("model", e.model.as_str()),
        ("srcvers", e.source_version.as_str()),
        ("protovers", "1.1"),
        ("pi", pairing),
        ("pk", key.as_str()),
    ];
    let mut service = ServiceInfo::new(
        "_airplay._tcp.local.",
        &e.name,
        &hostname,
        address.ip(),
        address.port(),
        &txt[..],
    )
    .map_err(error)?;
    service.set_interfaces(vec![IfKind::Addr(address.ip())]);
    Ok(service)
}
impl Discovery {
    pub fn start(c: &Credentials, address: SocketAddr) -> io::Result<Self> {
        let service = service(c, address)?;
        let fullname = service.get_fullname().to_owned();
        let daemon = ServiceDaemon::new().map_err(error)?;
        let setup = (|| {
            daemon.disable_interface(IfKind::All)?;
            daemon.enable_interface(IfKind::Addr(address.ip()))?;
            daemon.accept_unsolicited(false)?;
            daemon.set_ip_check_interval(30)?;
            let events = daemon.monitor()?;
            daemon.register(service)?;
            Ok::<_, mdns_sd::Error>(events)
        })();
        let events = match setup {
            Ok(e) => e,
            Err(e) => {
                let _ = daemon.shutdown();
                return Err(error(e));
            }
        };
        Ok(Self {
            daemon,
            events,
            fullname,
            address: address.ip(),
        })
    }
    pub async fn next(&self) -> io::Result<()> {
        match self.events.recv_async().await.map_err(error)? {
            DaemonEvent::Error(_) => Err(error("mDNS socket or registration failure")),
            DaemonEvent::IpDel(ip) if ip == self.address => Err(error(
                "configured local interface lost; explicit restart required",
            )),
            _ => Ok(()),
        }
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}
pub async fn next(discovery: &Option<Discovery>) -> io::Result<()> {
    match discovery {
        Some(d) => d.next().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use f50_core::{
        bplist,
        info::{self, DisplayInfo},
        mfi::Identity,
        wireless::Endpoint,
    };
    use std::sync::Arc;
    use zeroize::Zeroizing;
    struct UnusedIdentity;
    impl Identity for UnusedIdentity {
        fn certificate(&self) -> &[u8] {
            &[]
        }
        fn sign_digest(&self, _: &[u8; 32]) -> f50_core::Result<[u8; 64]> {
            panic!("discovery never uses accessory credentials")
        }
    }
    #[test]
    fn discovery_capabilities_match_the_info_response() {
        let info = info::build(&DisplayInfo {
            name: "synthetic",
            model: "F50",
            manufacturer: "test",
            device_id: "02:00:00:00:00:02",
            bluetooth_mac: "02:00:00:00:00:02",
            width: 1280,
            height: 720,
            fps: 30,
            width_mm: 180,
            hevc: false,
            microphone: false, right_hand_drive: false,
        })
        .unwrap();
        let mut value = bplist::decode(&info).unwrap();
        for features in [
            value.as_dictionary().unwrap()["features"]
                .as_unsigned_integer()
                .unwrap(),
            0xffffffff,
        ] {
            value
                .as_dictionary_mut()
                .unwrap()
                .insert("features".into(), bplist::number(features));
            let mut credentials = Credentials {
                pairing_store: None,
                pairing_id: b"synthetic-pairing".to_vec(),
                signing_seed: Zeroizing::new([0; 32]),
                mfi: Arc::new(UnusedIdentity),
                info: bplist::encode(&value).unwrap(),
                info_base: crate::receiver::InfoBase { name: "synthetic".into(), model: "F50".into(), manufacturer: "test".into(), device_id: "02:00:00:00:00:02".into(), bluetooth_mac: "02:00:00:00:00:02".into() },
                width: 1280,
                height: 720,
                endpoint: Some(Arc::new(Endpoint {
                    name: "synthetic".into(),
                    model: "F50".into(),
                    manufacturer: "test".into(),
                    serial: "synthetic".into(),
                    bluetooth_mac: [2, 0, 0, 0, 0, 2],
                    ssid: "synthetic-test".into(),
                    password: Zeroizing::new("synthetic-only".into()),
                    channel: 149,
                    security: 1,
                    addresses: vec!["192.0.2.1".parse().unwrap()],
                    airplay_port: 7000,
                    pairing_public_key: [0; 32],
                    source_version: "366.0".into(),
                })),
            };
            let registered = service(&credentials, "192.0.2.1:7000".parse().unwrap()).unwrap();
            let parts = registered
                .get_property_val_str("features")
                .unwrap()
                .split(',')
                .map(|part| u64::from_str_radix(part.trim_start_matches("0x"), 16).unwrap())
                .collect::<Vec<_>>();
            let advertised = parts[0] | (parts.get(1).copied().unwrap_or(0) << 32);
            assert_eq!(
                advertised, features,
                "discovery and /info must describe the same receiver"
            );
            assert_eq!(
                registered.get_property_val_str("deviceid"),
                Some("02:00:00:00:00:02")
            );
            assert_eq!(registered.get_property_val_str("srcvers"), Some("366.0"));
            assert!(registered.get_hostname().starts_with("cowplay-"));
            let pairing_id=credentials.pairing_id.clone();
            let seed=*credentials.signing_seed;
            let endpoint=Arc::get_mut(credentials.endpoint.as_mut().unwrap()).unwrap();
            endpoint.name="cowplay-rasp".into();
            let first=service(&credentials,"192.0.2.1:7000".parse().unwrap()).unwrap();
            let endpoint=Arc::get_mut(credentials.endpoint.as_mut().unwrap()).unwrap();
            endpoint.name="cowplay-garage".into();
            endpoint.pairing_public_key=[1;32];
            let second=service(&credentials,"192.0.2.2:7000".parse().unwrap()).unwrap();
            assert_ne!(first.get_fullname(),second.get_fullname());
            assert_ne!(first.get_hostname(),second.get_hostname());
            assert!(first.get_fullname().starts_with("cowplay-rasp."));
            assert_eq!(credentials.pairing_id,pairing_id);
            assert_eq!(*credentials.signing_seed,seed);
        }
    }
}
