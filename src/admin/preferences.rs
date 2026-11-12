use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------- persisted runtime preferences ----------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Prefs {
    pub ui: Ui,
    pub watchdog: Watchdog,
    pub log: LogPrefs,
    pub display: DisplayPrefs,
    pub audio: AudioPrefs,
    pub ddns: DdnsPrefs,
}
/// DDNS + certificate automation. The provider token is stored separately (write-only).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DdnsPrefs {
    pub enabled: bool,
    /// cloudflare | duckdns
    pub provider: String,
    pub domain: String,
    /// empty = this device's web address
    pub ip: String,
    pub interval_min: u32,
    /// renew the HTTPS certificate with Let's Encrypt DNS-01 when < 30 days remain
    pub cert: bool,
    pub email: String,
    pub staging: bool,
}
impl Default for DdnsPrefs {
    fn default() -> Self {
        Self { enabled: false, provider: "cloudflare".into(), domain: String::new(), ip: String::new(), interval_min: 30, cert: false, email: String::new(), staging: false }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AudioPrefs {
    /// car_bt: legacy name for no browser microphone | browser: car browser mic
    pub voice_route: String,
}
impl Default for AudioPrefs {
    fn default() -> Self {
        Self { voice_route: "car_bt".into() }
    }
}
/// Display declaration offered to the iPhone (PlayPort Display panel). Changing it restarts
/// the CarPlay session so the phone re-reads /info.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayPrefs {
    /// auto | smooth | sharp | custom
    pub quality: String,
    /// custom only; 0 = configured resolution
    pub width: u16,
    pub height: u16,
    /// custom only: 30 or 60
    pub fps: u8,
    /// larger | default | small | smaller (CarPlay control size via reported panel width)
    pub size: String,
    /// custom only: h264 | hevc
    pub codec: String,
}
impl Default for DisplayPrefs {
    fn default() -> Self {
        Self { quality: "auto".into(), width: 0, height: 0, fps: 30, size: "default".into(), codec: "h264".into() }
    }
}
pub const RESOLUTIONS: [(u16, u16); 4] = [(800, 480), (1280, 720), (1920, 720), (1920, 1080)];
pub(super) fn size_mm(size: &str) -> u16 {
    match size {
        "larger" => 150,
        "small" => 220,
        "smaller" => 260,
        _ => 180,
    }
}
/// Screen `ui_size` (small | std | large) as the legacy `display.size` name.
pub(super) fn display_size(ui_size: &str) -> &'static str {
    match ui_size {
        "small" => "small",
        "large" => "larger",
        _ => "default",
    }
}
/// Scale a picture region into a bounding box (never up), keeping aspect; even sides.
pub fn fit_box(width: f64, height: f64, max_w: f64, max_h: f64) -> Option<(u16, u16)> {
    if !(width.is_finite() && height.is_finite()) || width < 320.0 || height < 240.0 {
        return None;
    }
    let scale = (max_w / width).min(max_h / height).min(1.0);
    let even = |v: f64| ((v * scale / 2.0).floor() * 2.0) as u16;
    let (w, h) = (even(width), even(height));
    (w >= 320 && h >= 240).then_some((w, h))
}
/// Exact sizes must retain their aspect within the device envelope.
pub fn validate_screen_display(p:&f50_server::clients::ScreenPrefs,policy:&crate::policy::Policy)->Result<(),String>{
    if p.quality=="custom" {
        if let Some((w,h))=p.exact_resolution(){let (mw,mh)=policy.max_box();
            if fit_box(w as f64,h as f64,mw as f64,mh as f64).is_none(){return Err("此宽高比例无法在设备上限内满足最小尺寸；请调整宽高或设备上限。".into());}
        }
    }
    Ok(())
}
/// Policy built from the pre-UCI JSON settings, used once to create `/etc/config/cowplay`.
pub(super) fn legacy_policy(path: &Path) -> crate::policy::Policy {
    let mut policy = crate::policy::Policy::default();
    let Some(mut p) = std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<Prefs>(&b).ok()) else {
        return policy;
    };
    // Older settings allowed HEVC even though it was never declared. Preserve the
    // rest of that valid legacy configuration while normalizing this inert value.
    if p.display.codec == "hevc" { p.display.codec = "h264".into(); }
    if p.validate().is_err() { return policy; }
    policy.stall_ms = [1500, 2000, 3000, 5000].into_iter().min_by_key(|v| (*v as i64 - p.watchdog.stall_ms as i64).abs()).unwrap_or(2000);
    policy.retries = [1, 3, 5].into_iter().min_by_key(|v| (*v as i16 - p.watchdog.retries as i16).abs()).unwrap_or(3);
    policy.log_syslog = p.log.syslog;
    policy.log_store = if p.log.usb { "usb" } else { "ram" }.into();
    policy.log_usb_path = p.log.usb_path.clone();
    policy.ddns_enabled = p.ddns.enabled;
    if p.ddns.enabled { policy.ddns_mode="builtin".into(); }
    policy.ddns_provider = p.ddns.provider.clone();
    policy.domain = p.ddns.domain.clone();
    policy.ddns_ip = p.ddns.ip.clone();
    policy.ddns_email = p.ddns.email.clone();
    policy.ddns_staging = p.ddns.staging;
    policy.cert_source = if p.ddns.cert { "acme" } else { "upload" }.into();
    policy.ddns_interval = [5, 10, 30, 60].into_iter().min_by_key(|v| (*v as i64 - p.ddns.interval_min as i64).abs()).unwrap_or(10);
    policy.defaults.rail_side = p.ui.rail_side.clone();
    policy.defaults.call = if p.audio.voice_route == "browser" { "web" } else { "bt" }.into();
    policy.defaults.quality = if p.display.quality == "custom" { "auto".into() } else { p.display.quality.clone() };
    policy
}
/// Fit physical window pixels into the 1920×1080 envelope the receiver accepts, keeping aspect.
pub fn fit_window(width: f64, height: f64) -> Option<(u16, u16)> {
    fit_box(width, height, 1920.0, 1080.0)
}
/// Largest standard resolution (by area) that fits inside the car display's physical pixels.
pub fn best_preset((w, h): (u16, u16)) -> Option<(u16, u16)> {
    RESOLUTIONS.iter().copied().filter(|&(pw, ph)| pw <= w && ph <= h).max_by_key(|&(pw, ph)| pw as u32 * ph as u32)
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// "left" | "right"
    pub rail_side: String,
    pub hud: bool,
    /// CarPlay night appearance, pushed to the phone with `setNightMode`.
    pub night: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Watchdog {
    pub stall_ms: u32,
    pub retries: u8,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LogPrefs {
    pub syslog: bool,
    pub usb: bool,
    pub usb_path: String,
}
impl Default for Prefs {
    fn default() -> Self {
        Self {
            ui: Ui::default(),
            watchdog: Watchdog::default(),
            log: LogPrefs::default(),
            display: DisplayPrefs::default(),
            audio: AudioPrefs::default(),
            ddns: DdnsPrefs::default(),
        }
    }
}
impl Default for Ui {
    fn default() -> Self {
        Self {
            rail_side: "left".into(),
            hud: false,
            night: false,
        }
    }
}
impl Default for Watchdog {
    fn default() -> Self {
        Self {
            stall_ms: 2000,
            retries: 3,
        }
    }
}
impl Default for LogPrefs {
    fn default() -> Self {
        Self {
            syslog: false,
            usb: false,
            usb_path: "/mnt/sda1/cowplay-logs".into(),
        }
    }
}
impl Prefs {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.ui.rail_side.as_str(), "left" | "right") {
            return Err("ui.rail_side 只能是 left 或 right".into());
        }
        if !(1000..=10000).contains(&self.watchdog.stall_ms) {
            return Err("watchdog.stall_ms 需在 1000–10000".into());
        }
        if !(1..=5).contains(&self.watchdog.retries) {
            return Err("watchdog.retries 需在 1–5".into());
        }
        if self.log.usb {
            external_log_dir(&self.log.usb_path, false)?;
        }
        let d = &self.display;
        if !matches!(d.quality.as_str(), "auto" | "smooth" | "sharp" | "custom") {
            return Err("display.quality 只能是 auto/smooth/sharp/custom".into());
        }
        if !matches!(d.size.as_str(), "larger" | "default" | "small" | "smaller") {
            return Err("display.size 只能是 larger/default/small/smaller".into());
        }
        if d.codec!="h264" || !f50_core::info::FRAME_RATES.contains(&d.fps) {
            return Err("旧全局 display.codec 仅兼容 H.264；编码由屏幕偏好选择，帧率按实现范围校验，iPhone 完整能力未知".into());
        }
        let n = &self.ddns;
        if !matches!(n.provider.as_str(), "cloudflare" | "duckdns") || !(5..=1440).contains(&n.interval_min) {
            return Err("ddns.provider 只能是 cloudflare/duckdns，间隔 5–1440 分钟".into());
        }
        let host_ok = |d: &str| d.len() <= 253 && d.split('.').count() >= 2 && d.split('.').all(|l| !l.is_empty() && l.len() <= 63 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        if (n.enabled || n.cert) && !host_ok(&n.domain) {
            return Err("ddns.domain 不是有效域名".into());
        }
        if !n.ip.is_empty() && n.ip.parse::<std::net::Ipv4Addr>().is_err() {
            return Err("ddns.ip 必须为空（自动）或 IPv4 地址".into());
        }
        if n.email.len() > 128 || (!n.email.is_empty() && !n.email.contains('@')) {
            return Err("ddns.email 无效".into());
        }
        if !matches!(self.audio.voice_route.as_str(), "car_bt" | "browser") {
            return Err("audio.voice_route 只能是 car_bt 或 browser".into());
        }
        if !(d.width==0&&d.height==0) && crate::policy::resolution_box(&format!("{}x{}",d.width,d.height)).is_none() {
            return Err("display 分辨率超出接收端范围或不是偶数尺寸".into());
        }
        Ok(())
    }
}

/// Validate a log directory on external storage under /mnt. With `create`, make the
/// directory inside an existing mount point.
pub fn external_log_dir(path: &str, create: bool) -> Result<PathBuf, String> {
    let p = Path::new(path);
    let parts: Vec<_> = p.components().collect();
    use std::path::Component::*;
    if path.len() > 200
        || !p.is_absolute()
        || parts.len() < 3
        || parts[1] != Normal("mnt".as_ref())
        || parts[1..].iter().any(|c| !matches!(c, Normal(_)))
    {
        return Err("日志目录必须是 /mnt/<设备>/… 下的绝对路径".into());
    }
    let mount: PathBuf = parts[..3].iter().collect();

    // symlink_metadata: a symlinked mount point could lead back onto the flash overlay.
    let mount_meta = std::fs::symlink_metadata(&mount).map_err(|_| format!("{} 不存在，请先插入并挂载 U 盘", mount.display()))?;
    if !mount_meta.is_dir() {
        return Err(format!("{} 不是目录（或是符号链接）", mount.display()));
    }
    // A real mount can share a filesystem with /mnt (F50's ext4 root bind).
    // Check the exact mount record and persistent filesystem instead of st_dev.
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").map_err(|e| format!("无法核实日志挂载：{e}"))?;
    if !crate::persistent_logs::mounted_storage(&mounts, &mount) {
        return Err(format!("{} 不是已挂载、可写的持久存储", mount.display()));
    }
    let mut walk = mount.clone();
    for part in &parts[3..] {
        walk.push(part);
        match std::fs::symlink_metadata(&walk) {
            Ok(m) if m.file_type().is_symlink() => return Err(format!("{} 是符号链接，已拒绝", walk.display())),
            Ok(m) if !m.is_dir() => return Err(format!("{} 不是目录", walk.display())),
            Ok(_) => {}
            Err(_) if create => {
                { use std::os::unix::fs::DirBuilderExt; std::fs::DirBuilder::new().mode(0o700).create(&walk) }.map_err(|e| format!("无法创建日志目录：{e}"))?;
            }
            Err(_) => {}
        }
    }
    Ok(p.join("cowplay.log"))
}
