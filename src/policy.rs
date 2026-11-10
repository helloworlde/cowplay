//! Device-wide CowPlay policy stored in UCI `/etc/config/cowplay` (edited in LuCI).
//! Per-screen preferences live with each authorized screen (see `f50_server::clients`).
use crate::uci::Package;
use f50_server::clients::ScreenPrefs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_PATH: &str = "/etc/config/cowplay";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub enabled: bool,
    pub port: u16,
    pub domain: String,
    pub wifi_iface: String,
    /// Even WIDTHxHEIGHT in the receiver envelope; legacy aliases remain readable.
    pub max_res: String,
    pub max_fps: u8,
    /// auto | 4 | 8 | 12 (Mbps)
    pub bitrate: String,
    pub stall_ms: u32,
    pub retries: u8,
    pub stall_card: bool,
    pub auto_downgrade: bool,
    pub token_required: bool,
    /// 30d | forever
    pub token_remember: String,
    pub preferred: String,
    pub ask_when_multi: bool,
    /// seconds; 0 = wait forever
    pub ask_timeout: u8,
    pub redirect_http: bool,
    /// acme | upload | self | http
    pub cert_source: String,
    pub ddns_enabled: bool,
    /// builtin | luci
    pub ddns_mode: String,
    pub ddns_provider: String,
    pub ddns_interval: u32,
    pub ddns_ip: String,
    pub ddns_email: String,
    pub ddns_staging: bool,
    /// info | debug | trace
    pub log_level: String,
    /// ram | usb
    pub log_store: String,
    pub log_usb_path: String,
    pub log_keep: u8,
    pub log_syslog: bool,
    pub defaults: ScreenPrefs,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum Store {
    Uci { path: PathBuf },
    Json { path: PathBuf },
}
impl Store {
    pub fn path(&self) -> &Path {
        match self { Self::Uci { path } | Self::Json { path } => path }
    }
    pub fn read(&self) -> Result<Package, String> {
        match self {
            Self::Uci { path } => Package::read(path),
            Self::Json { path } => {
                let bytes = f50_server::settings::read(path.to_str().ok_or("invalid policy path")?, 32768).map_err(|e| e.to_string())?;
                Self::json_package(&bytes)
            }
        }
    }
    pub fn json_package(bytes: &[u8]) -> Result<Package, String> {
        let policy: Policy = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let mut pkg = Package::default();
        policy.to_uci(&mut pkg);
        Policy::validate_display(&pkg)?;
        if Policy::from_uci(&pkg) != policy {
            return Err("JSON 策略含无效或不支持的选项值".into());
        }
        Ok(pkg)
    }
    pub fn write(&self, pkg: &Package) -> Result<(), String> {
        Policy::validate_display(pkg)?;
        match self {
            Self::Uci { path } => pkg.write(path),
            Self::Json { path } => {
                let bytes = serde_json::to_vec_pretty(&Policy::from_uci(pkg)).map_err(|e| e.to_string())?;
                f50_server::settings::write_private(path, &bytes)
            }
        }.map_err(|e| e.to_string())
    }
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 18443,
            domain: String::new(),
            wifi_iface: String::new(),
            max_res: "1080p".into(),
            max_fps: 60,
            bitrate: "auto".into(),
            stall_ms: 2000,
            retries: 3,
            stall_card: true,
            auto_downgrade: false,
            token_required: true,
            token_remember: "forever".into(),
            preferred: String::new(),
            ask_when_multi: true,
            ask_timeout: 5,
            redirect_http: true,
            cert_source: "acme".into(),
            ddns_enabled: false,
            ddns_mode: "luci".into(),
            ddns_provider: "cloudflare".into(),
            ddns_interval: 10,
            ddns_ip: String::new(),
            ddns_email: String::new(),
            ddns_staging: false,
            log_level: "info".into(),
            log_store: "ram".into(),
            log_usb_path: "/mnt/sda1/cowplay-logs".into(),
            log_keep: 7,
            log_syslog: true,
            defaults: ScreenPrefs::default(),
        }
    }
}
fn flag(v: Option<&str>, d: bool) -> bool {
    match v {
        Some("1" | "on" | "true" | "yes" | "enabled") => true,
        Some("0" | "off" | "false" | "no" | "disabled") => false,
        _ => d,
    }
}
fn pick(v: Option<&str>, allowed: &[&str], d: &str) -> String {
    v.filter(|v| allowed.contains(v)).unwrap_or(d).to_owned()
}
fn num<T: std::str::FromStr + Copy>(v: Option<&str>, allowed: &[T], d: T) -> T
where
    T: PartialEq,
{
    v.and_then(|v| v.parse::<T>().ok()).filter(|n| allowed.contains(n)).unwrap_or(d)
}
fn text(v: Option<&str>, max: usize, d: &str) -> String {
    v.filter(|v| v.len() <= max && !v.chars().any(char::is_control)).unwrap_or(d).to_owned()
}
fn b(v: bool) -> &'static str {
    if v { "1" } else { "0" }
}
pub fn resolution_box(value:&str)->Option<(u32,u32)> {
    use f50_core::info::{MIN_WIDTH,MIN_HEIGHT,MAX_WIDTH,MAX_HEIGHT};
    let (w,h)=match value {
        "720p"=>(1280,720),"1080p"=>(1920,1080),
        "1440p"=>(2560,1440),"2160p"=>(3840,2160),
        _=>{let (w,h)=value.split_once('x')?;(w.parse().ok()?,h.parse().ok()?)}
    };
    ((MIN_WIDTH as u32..=MAX_WIDTH as u32).contains(&w)&&(MIN_HEIGHT as u32..=MAX_HEIGHT as u32).contains(&h)&&w%2==0&&h%2==0).then_some((w,h))
}
impl Policy {
    pub fn validate_display(p:&Package)->Result<(),String> {
        if p.get("main","max_res").is_some_and(|v|resolution_box(v).is_none()) {
            return Err("请求分辨率超出当前接收端范围；按返回边界填写偶数宽x高，iPhone 完整支持范围未知。".into());
        }
        if p.get("main","max_fps").is_some_and(|v|v.parse::<u8>().ok().is_none_or(|n|!f50_core::info::FRAME_RATES.contains(&n))) {
            return Err("帧率不在当前协议实现可声明的范围内".into());
        }
        Ok(())
    }
    pub fn from_uci(p: &Package) -> Self {
        let d = Self::default();
        let m = |k: &str| p.get("main", k);
        let s = |k: &str| p.get("screen", k);
        let defaults = ScreenPrefs {
            quality: pick(s("quality"), &["auto", "smooth", "sharp"], &d.defaults.quality),
            call: pick(s("call"), &["bt", "web"], &d.defaults.call),
            rail_side: pick(s("rail_side"), &["left", "right"], &d.defaults.rail_side),
            ..ScreenPrefs::default()
        };
        Self {
            enabled: flag(m("enabled"), d.enabled),
            port: m("port").and_then(|v| v.parse().ok()).filter(|p| *p > 0).unwrap_or(d.port),
            domain: text(m("domain"), 253, "").to_ascii_lowercase(),
            wifi_iface: text(m("wifi_iface"), 32, ""),
            max_res:m("max_res").filter(|v|resolution_box(v).is_some()).unwrap_or(&d.max_res).to_owned(),
            max_fps:num(m("max_fps"),&f50_core::info::FRAME_RATES,d.max_fps),
            bitrate: pick(m("bitrate"), &["auto", "4", "8", "12"], &d.bitrate),
            stall_ms: num(m("stall_ms"), &[1500, 2000, 3000, 5000], d.stall_ms),
            retries: num(m("retries"), &[1, 3, 5], d.retries),
            stall_card: flag(m("stall_card"), d.stall_card),
            auto_downgrade: flag(m("auto_downgrade"), d.auto_downgrade),
            token_required: flag(m("token_required"), d.token_required),
            token_remember: pick(m("token_remember"), &["30d", "forever"], &d.token_remember),
            preferred: text(m("preferred"), 64, ""),
            ask_when_multi: flag(m("ask_when_multi"), d.ask_when_multi),
            ask_timeout: num(m("ask_timeout"), &[0, 5, 10], d.ask_timeout),
            redirect_http: flag(m("redirect_http"), d.redirect_http),
            cert_source: pick(m("cert_source"), &["acme", "upload", "self", "http"], &d.cert_source),
            ddns_enabled: flag(m("ddns_enabled"), d.ddns_enabled),
            ddns_mode: pick(m("ddns_mode"), &["builtin", "luci"], if flag(m("ddns_enabled"),false) {"builtin"} else {&d.ddns_mode}),
            ddns_provider: pick(m("ddns_provider"), &["cloudflare", "duckdns"], &d.ddns_provider),
            ddns_interval: num(m("ddns_interval"), &[5, 10, 30, 60], d.ddns_interval),
            ddns_ip: text(m("ddns_ip"), 64, ""),
            ddns_email: text(m("ddns_email"), 254, ""),
            ddns_staging: flag(m("ddns_staging"), d.ddns_staging),
            log_level: pick(m("log_level"), &["info", "debug", "trace"], &d.log_level),
            log_store: pick(m("log_store"), &["ram", "usb"], &d.log_store),
            log_usb_path: text(m("log_usb_path"), 200, &d.log_usb_path),
            log_keep: num(m("log_keep"), &[1, 7, 30], d.log_keep),
            log_syslog: flag(m("log_syslog"), d.log_syslog),
            defaults,
        }
    }
    /// Write every field back into `p` (unknown options are kept).
    pub fn to_uci(&self, p: &mut Package) {
        let mut m = |k: &str, v: String| p.set("cowplay", "main", k, &v);
        m("enabled", b(self.enabled).into());
        m("port", self.port.to_string());
        m("domain", self.domain.clone());
        m("wifi_iface", self.wifi_iface.clone());
        m("max_res", self.max_res.clone());
        m("max_fps", self.max_fps.to_string());
        m("bitrate", self.bitrate.clone());
        m("stall_ms", self.stall_ms.to_string());
        m("retries", self.retries.to_string());
        m("stall_card", b(self.stall_card).into());
        m("auto_downgrade", b(self.auto_downgrade).into());
        m("token_required", b(self.token_required).into());
        m("token_remember", self.token_remember.clone());
        m("preferred", self.preferred.clone());
        m("ask_when_multi", b(self.ask_when_multi).into());
        m("ask_timeout", self.ask_timeout.to_string());
        m("redirect_http", b(self.redirect_http).into());
        m("cert_source", self.cert_source.clone());
        m("ddns_enabled", b(self.ddns_enabled).into());
        m("ddns_mode", self.ddns_mode.clone());
        m("ddns_provider", self.ddns_provider.clone());
        m("ddns_interval", self.ddns_interval.to_string());
        m("ddns_ip", self.ddns_ip.clone());
        m("ddns_email", self.ddns_email.clone());
        m("ddns_staging", b(self.ddns_staging).into());
        m("log_level", self.log_level.clone());
        m("log_store", self.log_store.clone());
        m("log_usb_path", self.log_usb_path.clone());
        m("log_keep", self.log_keep.to_string());
        m("log_syslog", b(self.log_syslog).into());
        let mut s = |k: &str, v: &str| p.set("defaults", "screen", k, v);
        s("quality", &self.defaults.quality);
        s("call", &self.defaults.call);
        s("rail_side", &self.defaults.rail_side);
    }
    pub fn client_policy(&self) -> f50_server::clients::Policy {
        f50_server::clients::Policy { required: self.token_required, remember_days: (self.token_remember == "30d").then_some(30) }
    }
    /// Bounding box for the negotiated picture (physical pixels).
    pub fn max_box(&self) -> (u32, u32) {
        resolution_box(&self.max_res).unwrap_or((1920,1080))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_envelope_and_legacy_settings_stay_consistent() {
        for fps in f50_core::info::FRAME_RATES {
            let p=Package::parse(&format!("config cowplay 'main'\n option max_res '1280x800'\n option max_fps '{fps}'\n")).unwrap();
            Policy::validate_display(&p).unwrap();
            let policy=Policy::from_uci(&p);assert_eq!(policy.max_box(),(1280,800));assert_eq!(policy.max_fps,fps);
        }
        for size in ["3842x2160","1921x1080","320x239","bad"] {
            let p=Package::parse(&format!("config cowplay 'main'\n option max_res '{size}'\n")).unwrap();
            assert!(Policy::validate_display(&p).is_err());
        }
        for fps in [0,29,31,61,120] {
            let p=Package::parse(&format!("config cowplay 'main'\n option max_fps '{fps}'\n")).unwrap();
            assert!(Policy::validate_display(&p).is_err());
        }
        let p=Package::parse("config cowplay 'main'\n option max_res '1440p'\n option preferred '/org/bluez/hci0/dev_00_11_22_33_44_55'\n option ddns_enabled '1'\n").unwrap();
        Policy::validate_display(&p).unwrap();
        let legacy=Policy::from_uci(&p);assert_eq!(legacy.max_box(),(2560,1440));
        assert!(legacy.ddns_enabled);assert!(legacy.preferred.ends_with("44_55"));
        assert_eq!(resolution_box("2160p"),Some((3840,2160)));
        assert_eq!(resolution_box("3840x2160"),Some((3840,2160)));
    }
    #[test]
    fn defaults_validation_and_round_trip() {
        let port = Policy::from_uci(&Package::parse("config cowplay 'main'\n option port '443'\n option ddns_enabled '1'\n").unwrap());
        assert_eq!(port.port, 443);
        assert_eq!(port.ddns_mode, "builtin", "existing enabled DDNS without a mode retains its updater");
        assert_eq!(Policy::default().ddns_mode, "luci");
        let p = Package::parse("config cowplay 'main'\n\toption max_res '720p'\n\toption retries '9'\n\toption token_remember '30d'\n\toption custom 'kept'\nconfig defaults 'screen'\n\toption quality 'sharp'\n").unwrap();
        let policy = Policy::from_uci(&p);
        assert_eq!(policy.max_res, "720p");
        assert_eq!(policy.retries, 3, "invalid value falls back to the default");
        assert_eq!(policy.defaults.quality, "sharp");
        assert_eq!(policy.client_policy().remember_days, Some(30));
        let mut out = p.clone();
        policy.to_uci(&mut out);
        assert_eq!(out.get("main", "custom"), Some("kept"));
        assert_eq!(Policy::from_uci(&out), policy);
        assert_eq!(Policy::from_uci(&Package::default()), Policy::default());
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;
    #[test]
    fn json_round_trip_is_private_and_invalid_values_never_overwrite_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.json");
        let store = Store::Json { path: path.clone() };
        let mut pkg = Store::json_package(br#"{"max_res":"1280x800","max_fps":30,"ddns_mode":"builtin"}"#).unwrap();
        store.write(&pkg).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let before = std::fs::read(&path).unwrap();
        assert_eq!(Policy::from_uci(&store.read().unwrap()).max_box(), (1280,800));
        pkg.set("cowplay", "main", "max_fps", "120");
        assert!(store.write(&pkg).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        for bytes in [br#"{"port":0}"#.as_slice(), br#"{"max_fps":120}"#, br#"{"unknown":true}"#, br#"{"log_level":"invalid"}"#] {
            assert!(Store::json_package(bytes).is_err());
        }
    }
}
