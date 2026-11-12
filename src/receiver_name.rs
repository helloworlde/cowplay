//! One friendly name for Bluetooth, iAP2, /info and DNS-SD; pairing identity stays separate.
use std::sync::OnceLock;

const PREFIX: &str = "cowplay-";
const MAX_BYTES: usize = 63; // DNS-SD instance label; also fits Bluetooth Local Name/EIR.

pub(crate) fn local_device_name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| host_label(&std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default()))
}

fn host_label(hostname: &str) -> String {
    let host: String = hostname.trim().split('.').next().unwrap_or("").chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .take(MAX_BYTES - PREFIX.len()).collect();
    let host = host.trim_matches('-');
    if host.is_empty() { "device".into() } else { host.into() }
}

pub(crate) fn for_device(hostname: &str) -> String {
    format!("{PREFIX}{}", host_label(hostname))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn name_is_only_product_and_device_name() {
        assert_eq!(for_device("rasp\n"), "cowplay-rasp");
        assert_eq!(for_device("Rasp.local"), "cowplay-rasp");
        assert_eq!(for_device("garage"), "cowplay-garage");
        assert_eq!(for_device("rasp"), for_device("rasp"));
    }
    #[test]
    fn long_hostnames_fit_every_advertisement() {
        let name = for_device(&"rasp".repeat(30));
        assert_eq!(name.len(), MAX_BYTES);
        assert!(name.starts_with("cowplay-rasp"));
    }
    #[test]
    fn invalid_host_label_has_a_readable_fallback() {
        assert_eq!(for_device("\0\n"), "cowplay-device");
        assert_eq!(for_device("---"), "cowplay-device");
        assert_eq!(for_device("Garage/One"), "cowplay-garage-one");
    }
}
