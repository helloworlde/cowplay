//! Authenticated diagnostics and settings API used by the car, phone and admin pages.
//! Every value is read from the running system; anything not measurable is reported as unknown.
use crate::bluetooth::Manager;
use f50_server::{
    Hub,
    diag::{self, diag},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

mod certificates;
mod diagnostics;
mod display;
mod http;
mod preferences;
pub(crate) mod management;

pub use http::router;
pub(crate) use http::{fail, json_ok};
pub use preferences::{AudioPrefs, DdnsPrefs, DisplayPrefs, LogPrefs, Prefs, RESOLUTIONS, Ui, Watchdog, best_preset, external_log_dir, fit_box, fit_window, validate_screen_display};
use preferences::{display_size, legacy_policy};
pub(crate) use crate::platform::host::{default_route, iw_interfaces, local_addresses, run_bounded};
#[cfg(test)]
use axum::http::StatusCode;

fn log_storage_check(enabled: bool, storage: &Value) -> (&'static str, String) {
    if !enabled { return ("warn", "当前仅RAM日志，服务重启后不会保留".into()); }
    if storage["failures"].as_u64().unwrap_or(0)>0 {
        return ("fail", storage["error"].as_str().unwrap_or("持久日志写入失败").to_owned());
    }
    if storage["active"]!=true { return ("fail", "持久日志已配置，但写入线程不可用".into()); }
    let synced=storage["last_sync_ms"].as_u64().unwrap_or(0);
    if synced==0 { return ("warn", "日志已打开，等待首次实盘同步完成".into()); }
    if storage["sync_age_ms"].as_u64().is_none_or(|age| age>30_000) { return ("warn", "最近实盘同步已过30秒或时间未知，请检查存储是否阻塞".into()); }
    let dropped=storage["queue_dropped"].as_u64().unwrap_or(0);
    (if dropped>0 {"warn"} else {"ok"}, format!("目标每秒同步；队列丢失 {dropped} 行；最近成功同步 {synced} ms UTC"))
}

// ---------- shared state ----------

pub struct Admin {
    pub manager: Arc<Manager>,
    pub hub: Arc<Hub>,
    pub origin: String,
    pub uci_path: PathBuf,
    pub policy_store: crate::policy::Store,
    pub openwrt: bool,
    pub config_path: PathBuf,
    pub state_dir: PathBuf,
    pub(crate) tls: Mutex<(String, String)>,
    pub web_bind: SocketAddr,
    pub control_bind: SocketAddr,
    pub wifi: Option<(String, u8)>,
    pub(crate) active_wifi_password: Mutex<Option<zeroize::Zeroizing<String>>>,
    pub display: (u16, u16),
    policy: Mutex<Option<crate::policy::Policy>>,
    pub selftest: Mutex<Option<Value>>,
    pub(crate) ddns_state: Mutex<DdnsState>,
    pub(crate) ddns_busy: Arc<tokio::sync::Semaphore>,
    pub(crate) connection_check: Mutex<Option<Value>>,
    pub(crate) identity_pending: std::sync::atomic::AtomicBool,
    pub(crate) sys: Mutex<SysState>,
    /// A settings-triggered phone re-invite is in flight.
    reinviting: Arc<std::sync::atomic::AtomicBool>,
    /// The user disconnected CarPlay; screens stop calling a phone until someone connects again.
    pub(crate) stopped: std::sync::atomic::AtomicBool,
    /// The last successfully cancelled pairing selection; zero means no cancellation.
    pub(crate) cancelled_pairing_epoch: std::sync::atomic::AtomicU64,
    /// Last-used time per saved iPhone (`phones-used.json`), loaded on first use.
    pub(crate) phone_usage: Mutex<Option<std::collections::BTreeMap<String, u64>>>,
    auto_fps: Mutex<crate::auto_fps::AutoFps>,
}
#[derive(Default, Clone, Serialize)]
pub(crate) struct DdnsState {
    pub(crate) last_update_ms: Option<u64>,
    last_update: Option<String>,
    pub(crate) update_ok: Option<bool>,
    cert_progress: Option<String>,
    cert_ms: Option<u64>,
    cert_result: Option<String>,
    cert_ok: Option<bool>,
    running: bool,
}
#[derive(Default)]
pub(crate) struct SysState {
    health_bucket: u64,
    cpu_prev: Option<(u64, u64)>,
    proc_prev: Option<(u64, Instant)>,
    cpu: Option<f64>,
    proc_cpu: Option<f64>,
    peak_rss_kb: u64,
    versions: Option<Value>,
    pub(crate) phone: Option<(String, Option<String>, Option<i32>, Instant)>,
    pub(crate) wifi: Option<(Value, Instant)>,
}

impl Admin {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        manager: Arc<Manager>,
        hub: Arc<Hub>,
        origin: String,
        uci_path: PathBuf,
        config_path: PathBuf,
        state_dir: PathBuf,
        tls_certificate: String,
        tls_private_key: String,
        web_bind: SocketAddr,
        control_bind: SocketAddr,
        wifi: Option<(String, u8)>,
        display: (u16, u16),
    ) -> Result<Arc<Self>, String> {
        Self::with_store(manager, hub, origin, crate::policy::Store::Uci { path: uci_path }, config_path, state_dir, tls_certificate, tls_private_key, web_bind, control_bind, wifi, display, true)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn with_store(
        manager: Arc<Manager>, hub: Arc<Hub>, origin: String, policy_store: crate::policy::Store,
        config_path: PathBuf, state_dir: PathBuf, tls_certificate: String, tls_private_key: String,
        web_bind: SocketAddr, control_bind: SocketAddr, wifi: Option<(String, u8)>, display: (u16, u16), openwrt: bool,
    ) -> Result<Arc<Self>, String> {
        let uci_path = policy_store.path().to_owned();
        let admin = Arc::new(Self {
            manager,
            hub,
            origin,
            uci_path,
            policy_store,
            openwrt,
            config_path,
            state_dir,
            tls: Mutex::new((tls_certificate, tls_private_key)),
            web_bind,
            control_bind,
            wifi,
            active_wifi_password: Mutex::new(None),
            display,
            policy: Mutex::new(None),
            selftest: Mutex::new(None),
            ddns_state: Mutex::new(DdnsState::default()),
            ddns_busy: Arc::new(tokio::sync::Semaphore::new(1)),
            connection_check: Mutex::new(None),
            identity_pending: std::sync::atomic::AtomicBool::new(false),
            sys: Mutex::new(SysState::default()),
            reinviting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            stopped: std::sync::atomic::AtomicBool::new(false),
            cancelled_pairing_epoch: std::sync::atomic::AtomicU64::new(0),
            phone_usage: Mutex::new(None),
            auto_fps: Mutex::new(crate::auto_fps::AutoFps::default()),
        });
        admin.reload()?;
        admin.apply_display(false);
        Ok(admin)
    }
    /// Re-read `/etc/config/cowplay` (first start: migrate the legacy JSON settings) and apply it.
    pub fn reload(&self) -> Result<crate::policy::Policy, String> {
        let policy = if self.uci_path.exists() {
            {let pkg=self.policy_store.read()?;
             crate::policy::Policy::validate_display(&pkg)?; crate::policy::Policy::from_uci(&pkg)}
        } else {
            let mut policy = legacy_policy(&self.state_dir.join("ui-settings.json"));
            if !self.openwrt || matches!(self.policy_store, crate::policy::Store::Json { .. }) {
                policy.port = self.web_bind.port();
                policy.ddns_mode = "builtin".into();
            }
            let mut pkg = crate::uci::Package::default();
            policy.to_uci(&mut pkg);
            if let Err(e) = self.policy_store.write(&pkg) {
                f50_server::diag_log!(Warn, "web", "cannot create {}: {e}", self.uci_path.display());
            }
            policy
        };
        let screens:Vec<_>=self.hub.clients.list().into_iter().filter(|c|c.kind=="screen").collect();
        if let Some(error)=screens.iter().find_map(|c|validate_screen_display(&c.prefs,&policy).err()) {
            // LuCI commits UCI before reload. Restore only the rejected display limits,
            // so a subsequent restart cannot silently select another aspect ratio.
            if let Some(previous)=self.policy.lock().unwrap().clone() {
                if screens.iter().all(|c|validate_screen_display(&c.prefs,&previous).is_ok()) {
                    let mut pkg=self.policy_store.read()?;
                    pkg.set("cowplay","main","max_res",&previous.max_res);
                    pkg.set("cowplay","main","max_fps",&previous.max_fps.to_string());
                    self.policy_store.write(&pkg).map_err(|e|format!("{error} 恢复原画面上限失败：{e}"))?;
                    return Err(format!("{error} 画面上限已恢复为上次有效值。"));
                }
            }
            return Err(error);
        }
        self.hub.clients.set_policy(policy.client_policy(), policy.defaults.clone());
        *self.policy.lock().unwrap() = Some(policy.clone());
        if !policy.auto_downgrade { self.auto_fps.lock().unwrap().sync(None); }
        let prefs = self.prefs();
        if let Err(e) = self.apply(&prefs) {
            f50_server::diag_log!(Warn, "web", "log settings not applied: {e}");
        }
        diag().set_verbose(policy.log_level != "info");
        Ok(policy)
    }
    pub fn policy(&self) -> crate::policy::Policy {
        self.policy.lock().unwrap().clone().unwrap_or_default()
    }
    /// Change device policy options (phone toggles, LuCI-less fallbacks) and persist them to UCI.
    pub fn update_policy(&self, change: impl FnOnce(&mut crate::policy::Policy)) -> Result<crate::policy::Policy, String> {
        let mut pkg = self.policy_store.read()?;
        let mut policy = crate::policy::Policy::from_uci(&pkg);
        change(&mut policy);
        for client in self.hub.clients.list().iter().filter(|c|c.kind=="screen") { validate_screen_display(&client.prefs,&policy)?; }
        policy.to_uci(&mut pkg);
        self.policy_store.write(&pkg).map_err(|e| format!("保存失败：{e}"))?;
        self.reload()
    }
    /// Preferences of the screen that currently shows CarPlay.
    pub fn viewer(&self) -> Option<f50_server::clients::Client> {
        self.hub.viewer_client().and_then(|id| self.hub.clients.get(&id))
    }
    /// The Wi‑Fi password from the configured handoff password file (QR code on the car).
    pub fn wifi_password(&self) -> Option<String> {
        if let Some(password) = self.active_wifi_password.lock().unwrap().as_ref() { return Some(password.to_string()); }
        let config: Value = serde_json::from_slice(&std::fs::read(&self.config_path).ok()?).ok()?;
        let file = config.pointer("/receiver/wifi/password_file")?.as_str()?;
        let pw = std::fs::read_to_string(file).ok()?;
        let pw = pw.trim_end_matches(['\n', '\r']).to_owned();
        (pw.len() <= 63).then_some(pw)
    }
    pub(crate) fn user_stopped(&self)->bool {
        if self.stopped.load(std::sync::atomic::Ordering::Acquire){return true;}
        let selection=self.hub.phones.snapshot();
        let cancelled=self.cancelled_pairing_epoch.load(std::sync::atomic::Ordering::Acquire);
        cancelled!=0&&cancelled==selection.epoch&&!selection.admission
    }
    /// "断开 CarPlay" from any page: publish the stop first (screens poll while the old link
    /// closes), then end Bluetooth and CarPlay, keeping the pairing. A failure restores the
    /// previous stop state.
    pub async fn disconnect(&self) -> crate::RuntimeResult<()> {
        let was = self.stopped.swap(true, std::sync::atomic::Ordering::AcqRel);
        if let Err(e) = crate::phone_flow::disconnect(&self.manager, &self.hub).await {
            self.stopped.store(was, std::sync::atomic::Ordering::Release);
            return Err(e);
        }
        diag().event("info", "bt", "已断开 CarPlay（配对保留）");
        Ok(())
    }
    /// End the live CarPlay session and invite the same phone again over Bluetooth.
    pub fn reinvite(&self, reason: &str) {
        if self.stopped.load(std::sync::atomic::Ordering::Acquire) { return; }
        let selection=self.hub.phones.snapshot();
        if selection.admission && self.reinviting.compare_exchange(false,true,std::sync::atomic::Ordering::AcqRel,std::sync::atomic::Ordering::Acquire).is_ok() {
            if !self.hub.end_session() { self.reinviting.store(false,std::sync::atomic::Ordering::Release); return; }
            self.reinvite_task(reason, selection.epoch);
        }
    }
    fn reinvite_task(&self, reason: &str, epoch: u64) {
        diag().event_detail("info", "carplay", "重新连接 iPhone", reason.to_owned());
            let manager = self.manager.clone();
            let hub = self.hub.clone();
            let admin_flag = self.reinviting.clone();
            tokio::spawn(async move {
                async {
                    let _guard=hub.phones.operation.lock().await;
                    if !crate::phone_flow::wait_current(&hub,epoch,Duration::from_millis(1500)).await {return;}
                    tokio::select! {biased;_=hub.phones.changed_since(epoch)=>return,_=manager.disconnect_phone()=>{}}
                    if !hub.phones.is_current(epoch) {return;}
                    for _ in 0..50 {
                        if !manager.status().rfcomm {break;}
                        if !crate::phone_flow::wait_current(&hub,epoch,Duration::from_millis(100)).await {return;}
                    }
                    let mut last=String::new();
                    for attempt in 1..=3 {
                        if !crate::phone_flow::wait_current(&hub,epoch,Duration::from_millis(3000)).await {return;}
                        if hub.phone_verified.load(std::sync::atomic::Ordering::Acquire) {return;}
                        diag().link_step("bluetooth","active",format!("设置已变更，重新连接 iPhone（第 {attempt}/3 次）"));
                        match crate::phone_flow::invite_locked(&manager,&hub,epoch).await {
                            Ok(())=>return,
                            Err(error)=>last=error.to_string(),
                        }
                    }
                    if hub.phones.is_current(epoch) && !last.is_empty() {diag().link_fail("bluetooth","设置已生效，但重新连接 iPhone 失败","点“连接 iPhone”再试一次；新设置已保存。");}
                }.await;
                admin_flag.store(false,std::sync::atomic::Ordering::Release);
            });
    }
    /// Apply logging preferences. Returns an error when the requested USB sink cannot be used.
    fn apply(&self, p: &Prefs) -> Result<(), String> {
        diag().set_mirror(p.log.syslog);
        let sink = if p.log.usb {
            match external_log_dir(&p.log.usb_path, true) {
                Ok(file) => Some(file),
                Err(e) => {
                    f50_server::diag_log!(Warn, "web", "USB log sink unavailable: {e}");
                    diag().sink_failed(&e);
                    return Err(e);
                }
            }
        } else {
            None
        };
        let result = match sink {
            Some(path) => crate::persistent_logs::open_log(&path).and_then(|file| diag().set_sink_file(path, file, self.policy().log_keep)),
            None => diag().set_sink(None),
        };
        result.map_err(|e| {
            diag().sink_failed(&e);
            f50_server::diag_log!(Warn, "web", "USB log sink open failed: {e}");
            format!("无法打开日志文件：{e}")
        })
    }
    /// Effective runtime preferences: device policy plus the active (or default) screen.
    pub fn prefs(&self) -> Prefs {
        let p = self.policy();
        let screen = self.viewer().map(|v| v.prefs).unwrap_or_else(|| p.defaults.clone());
        Prefs {
            ui: Ui { rail_side: screen.rail_side.clone(), hud: screen.debug, night: screen.night },
            watchdog: Watchdog { stall_ms: p.stall_ms, retries: p.retries },
            log: LogPrefs { syslog: p.log_syslog, usb: p.log_store == "usb", usb_path: p.log_usb_path.clone() },
            display: DisplayPrefs { quality: screen.quality.clone(), size: display_size(&screen.ui_size).into(), ..DisplayPrefs::default() },
            audio: AudioPrefs { voice_route: if screen.call == "web" { "browser" } else { "car_bt" }.into() },
            ddns: DdnsPrefs { enabled: p.ddns_enabled && p.ddns_mode == "builtin", provider: p.ddns_provider.clone(), domain: if p.domain.is_empty() { reqwest::Url::parse(&self.origin).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_default() } else { p.domain.clone() }, ip: p.ddns_ip.clone(), interval_min: p.ddns_interval, cert: p.cert_source == "acme", email: p.ddns_email.clone(), staging: p.ddns_staging },
        }
    }


}

pub(crate) fn summary(s: &diag::Session) -> Value {
    json!({
        "id": s.id,
        "started_ms": s.started_ms,
        "ended_ms": s.ended_ms,
        "phone": s.phone,
        "peer": s.peer,
        "end_reason": s.end_reason,
        "stalls": s.stalls,
        "warnings": s.warnings,
        "first_frame_ms": s.first_frame_ms,
        "events": s.events,
    })
}

pub fn unsupported() -> Value {
    json!({
        "wired": "F50 的 USB 口不能当主机（硬件限制），无法有线 CarPlay；只能无线重连",
    })
}

pub fn duration_text(s: u64) -> String {
    let (d, h, m) = (s / 86400, (s / 3600) % 24, (s / 60) % 60);
    if d > 0 {
        format!("{d} 天 {h} 小时 {m} 分")
    } else if h > 0 {
        format!("{h} 小时 {m} 分")
    } else {
        format!("{m} 分 {} 秒", s % 60)
    }
}

// ---------- TLS certificate validity (shared RustCrypto X.509 parser) ----------

pub fn cert_not_after(der_cert: &[u8]) -> Option<i64> {
    use der::Decode;
    let cert = x509_cert::Certificate::from_der(der_cert).ok()?;
    i64::try_from(cert.tbs_certificate.validity.not_after.to_unix_duration().as_secs()).ok()
}
pub(crate) fn tls_info(path: &str) -> Result<(i64, usize), String> {
    let text = f50_server::settings::read(path, 256 * 1024).map_err(|_| "证书文件不可读".to_owned())?;
    let certs = x509_cert::Certificate::load_pem_chain(&text).map_err(|_| "证书编码无效".to_owned())?;
    let leaf = certs.first().ok_or("未找到证书")?;
    let after = leaf.tbs_certificate.validity.not_after.to_unix_duration().as_secs() as i64;
    Ok(((after - (diag::now_ms() / 1000) as i64).div_euclid(86400), certs.len()))
}

// ---------- config rendering ----------

fn redact(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| {
                    if k=="tesla_ble" {return (k,json!({"configured":!v.is_null()}));}
                    let secret = k.contains("token") || k.contains("password") && !k.ends_with("_file");
                    (k, if secret && !v.is_null() { json!("••••••••") } else { redact(v) })
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(redact).collect()),
        other => other,
    }
}
fn render_toml(out: &mut String, prefix: &str, v: &Value) {
    let Value::Object(m) = v else { return };
    let mut nested = Vec::new();
    let scalars: Vec<_> = m.iter().filter(|(_, v)| !v.is_object()).collect();
    if !scalars.is_empty() && !prefix.is_empty() {
        out.push_str(&format!("[{prefix}]\n"));
    }
    for (k, v) in scalars {
        out.push_str(&format!("{k:<9} = {v}\n"));
    }
    for (k, v) in m.iter().filter(|(_, v)| v.is_object()) {
        nested.push((k, v));
    }
    for (k, v) in nested {
        let p = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        out.push('\n');
        render_toml(out, &p, v);
    }
}

pub(crate) fn stamp() -> String {
    let s = diag::now_ms() / 1000;
    let days = (s / 86400) as i64;
    // civil from days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}{m:02}{d:02}-{:02}{:02}", (s / 3600) % 24, (s / 60) % 60)
}

/// Name of the saved (bonded) iPhone from BlueZ storage: Alias, else Name. Display only.
fn saved_phone_name(state: &Path) -> Option<String> {
    let path = std::fs::read_to_string(state.join("phone")).ok()?;
    let mac = path.trim().rsplit("dev_").next()?.replace('_', ":");
    if mac.len() != 17 {
        return None;
    }
    for adapter in std::fs::read_dir(state.join("bluetooth")).ok()?.flatten() {
        let Ok(info) = std::fs::read_to_string(adapter.path().join(&mac).join("info")) else { continue };
        let field = |key: &str| info.lines().find_map(|l| l.strip_prefix(key)).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        if let Some(name) = field("Alias=").or_else(|| field("Name=")) {
            return Some(name.chars().take(64).collect());
        }
    }
    None
}
fn step_name(step: &str) -> &'static str {
    match step {
        "bluetooth" => "蓝牙",
        "wifi" => "Wi‑Fi",
        _ => "CarPlay",
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    async fn display_fixture() -> (Arc<Admin>, zbus::Connection, tokio::sync::mpsc::Receiver<f50_server::Control>, tempfile::TempDir) {
        let (manager, server, old, dir) = crate::bluetooth::recovery_tests::admin_fixture().await;
        let (hub, commands) = f50_server::Hub::with_phones("synthetic".into(), "https://test.invalid".into(), old.phones.clone());
        let a = Admin::new(manager,hub.clone(),"https://test.invalid".into(),dir.path().join("cowplay.uci"),dir.path().join("config"),dir.path().to_owned(),"cert".into(),"key".into(),"127.0.0.1:8080".parse().unwrap(),"127.0.0.1:7000".parse().unwrap(),None,(800,480)).expect("valid synthetic policy");
        let selected = hub.phones.snapshot();
        assert!(hub.phones.handoff(selected.epoch,selected.device.as_deref().unwrap()));
        (a,server,commands,dir)
    }
    #[tokio::test]
    async fn passive_window_changes_keep_the_live_declaration_and_do_not_end_the_phone() {
        let (a,_server,mut commands,_dir) = display_fixture().await;
        let original = a.hub.display_spec().unwrap();
        a.hub.phone_active.store(true,Ordering::Release);
        let resized = f50_server::DisplaySpec {width:1280,height:800,..original};
        a.commit_display(resized,false);
        assert_eq!(a.hub.display_spec(),Some(original),"resize must not rewrite an active phone's declaration");
        assert!(commands.try_recv().is_err(),"passive geometry must not end CarPlay");
        a.hub.phone_active.store(false,Ordering::Release);
        a.commit_display(resized,false);
        assert_eq!(a.hub.display_spec(),Some(resized),"next idle session uses the new window");
        assert!(commands.try_recv().is_err());
    }
    fn arm_auto_fps(a: &Admin, viewer: &f50_server::clients::Client) {
        let context = a.auto_context(&a.policy(), viewer).unwrap();
        let mut auto = a.auto_fps.lock().unwrap(); auto.sync(Some(context.clone()));
        let now = Instant::now();
        for sec in 0..6 { assert!(!auto.observe(now + Duration::from_secs(sec), (1, 1), true, 60.0, Some((sec, if sec % 2 == 0 {10.0} else {40.0})))); }
        assert!(auto.observe(now + Duration::from_secs(6), (1, 1), true, 60.0, Some((6, 10.0))));
        assert!(auto.accept(&context));
    }
    #[tokio::test]
    async fn auto_fps_changes_the_real_declaration_once_and_disabling_restores_the_request() {
        let (a,_server,mut commands,_dir) = display_fixture().await;
        assert!(!a.policy().auto_downgrade, "the policy remains opt-in");
        a.policy.lock().unwrap().as_mut().unwrap().auto_downgrade = true;
        let viewer = a.hub.clients.admit_token(&a.hub.clients.token(), "screen", "synthetic-screen", "", "").unwrap();
        let original = a.display_for(&a.policy(), Some(&viewer)).unwrap(); assert_eq!(original.fps, 60);
        a.hub.set_display_spec(original); a.hub.phone_active.store(true, Ordering::Release);
        arm_auto_fps(&a, &viewer);
        let capped = a.display_for(&a.policy(), Some(&viewer)).unwrap(); assert_eq!(capped.fps, 30);
        assert_eq!((capped.width,capped.height),(original.width,original.height));
        a.commit_display(capped, false);
        assert_eq!(a.hub.display_spec(), Some(capped)); assert_eq!(a.auto_fps_limit(), Some(30));
        assert!(matches!(commands.try_recv().unwrap(), f50_server::Control::EndSession));
        a.commit_display(capped, false); assert!(commands.try_recv().is_err());
        // The planned transport reconnect keeps the selection epoch and 30 fps decision.
        assert_eq!(a.display_for(&a.policy(), Some(&viewer)).unwrap().fps, 30);
        assert!(a.display_for(&a.policy(), None).is_none());
        assert_eq!(a.auto_fps.lock().unwrap().limit(), Some(30));
        a.policy.lock().unwrap().as_mut().unwrap().auto_downgrade = false;
        let restored = a.display_for(&a.policy(), Some(&viewer)).unwrap(); assert_eq!(restored, original);
        assert_eq!(a.auto_fps_limit(), None);
        a.hub.pause_phone().unwrap();
    }
    #[tokio::test]
    async fn stopping_or_advancing_the_phone_selection_discards_the_automatic_cap() {
        for stop in [true,false] {
            let (a,_server,mut commands,_dir) = display_fixture().await;
            a.policy.lock().unwrap().as_mut().unwrap().auto_downgrade = true;
            let viewer = a.hub.clients.admit_token(&a.hub.clients.token(), "screen", "synthetic-screen", "", "").unwrap();
            arm_auto_fps(&a, &viewer);
            a.hub.phone_active.store(true, Ordering::Release);
            if stop { a.stopped.store(true, Ordering::Release); }
            else { a.hub.pause_phone().unwrap(); }
            assert_eq!(a.display_for(&a.policy(), Some(&viewer)).unwrap().fps, 60);
            assert_eq!(a.auto_fps_limit(), None);
            a.reinvite("late automatic decision");
            assert!(commands.try_recv().is_err(), "stop or a newer selection cannot call the old phone back");
            assert!(!a.reinviting.load(Ordering::Acquire));
        }
    }
    #[tokio::test]
    async fn a_full_control_queue_preserves_the_original_declaration() {
        let (a,_server,mut commands,_dir) = display_fixture().await;
        let original = a.hub.display_spec().unwrap(); assert_eq!(original.fps,60);
        a.hub.phone_active.store(true, Ordering::Release);
        while a.hub.control.try_send(f50_server::Control::Keyframe).is_ok() {}
        assert!(!a.set_auto_spec(original));
        assert_eq!(a.hub.display_spec(),Some(original), "an unaccepted command cannot change the declared frame rate");
        assert!(!a.hub.take_planned_restart());
        while commands.try_recv().is_ok() {}
        assert!(a.set_auto_spec(original));
        assert_eq!(a.hub.display_spec().unwrap().fps,30);
        assert!(matches!(commands.try_recv().unwrap(),f50_server::Control::EndSession));
        assert!(commands.try_recv().is_err());
    }
    #[tokio::test]
    async fn repeated_reinvite_requests_emit_only_one_end_session() {
        let (a,_server,mut commands,_dir) = display_fixture().await;
        a.hub.phone_active.store(true,Ordering::Release);
        a.reinvite("explicit setting");
        a.reinvite("same pending transition");
        assert!(matches!(commands.try_recv().unwrap(), f50_server::Control::EndSession));
        assert!(commands.try_recv().is_err(),"a pending reinvite must not enqueue another end for the next session");
        a.hub.pause_phone().unwrap();
    }
    #[tokio::test]
    async fn explicit_geometry_and_real_profile_changes_still_end_exactly_one_session() {
        for explicit in [true,false] {
            let (a,_server,mut commands,_dir) = display_fixture().await;
            let original = a.hub.display_spec().unwrap();
            a.hub.phone_active.store(true,Ordering::Release);
            let requested = if explicit { f50_server::DisplaySpec {width:1280,height:800,..original} }
                else { f50_server::DisplaySpec {fps:30,microphone:true,..original} };
            a.commit_display(requested,explicit);
            assert_eq!(a.hub.display_spec(),Some(requested));
            assert!(matches!(commands.try_recv().unwrap(),f50_server::Control::EndSession));
            a.commit_display(requested,explicit);
            assert!(commands.try_recv().is_err());
            a.hub.pause_phone().unwrap();
        }
    }
    #[tokio::test]
    async fn ordinary_browser_cookie_cannot_access_luci_only_maintenance() {
        let (manager, _server, hub, dir) = crate::bluetooth::recovery_tests::admin_fixture().await;
        let browser = hub.clients.admit_token(&hub.clients.token(), "phone", "phone", "", "").unwrap();
        let a = Admin::new(manager, hub, "https://test.invalid".into(), dir.path().join("cowplay.uci"), dir.path().join("config"), dir.path().to_owned(), "cert".into(), "key".into(), "127.0.0.1:8080".parse().unwrap(), "127.0.0.1:7000".parse().unwrap(), None, (800, 480)).expect("valid synthetic policy");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(axum::serve(listener, router(a)).into_future());
        let client = reqwest::Client::new();
        for endpoint in ["/api/actions/reset-bluetooth", "/api/actions/restart-service", "/api/ddns/token", "/api/ddns/update", "/api/ddns/renew",
            "/api/actions/restart-video", "/api/actions/disconnect-phone", "/api/logs/verbose", "/api/selftest", "/api/diag/share", "/api/diag/share/revoke"] {
            let result = client.post(format!("http://{addr}{endpoint}")).header("Origin", "https://test.invalid").header("Cookie", format!("{}={}", f50_server::clients::COOKIE, browser.id)).header("Content-Type", "application/json").body(r#"{"token":"test-only"}"#).send().await.unwrap();
            assert_eq!(result.status(), StatusCode::NOT_FOUND, "maintenance must require LuCI: {endpoint}");
        }
        // Former /admin page reads, including the unauthenticated share download, are gone.
        for endpoint in ["/api/overview", "/api/metrics", "/api/sessions", "/api/selftest", "/api/process", "/api/config", "/api/diag/blob", "/share/ABC-DEF"] {
            let result = client.get(format!("http://{addr}{endpoint}")).header("Cookie", format!("{}={}", f50_server::clients::COOKIE, browser.id)).send().await.unwrap();
            assert_eq!(result.status(), StatusCode::NOT_FOUND, "removed endpoint: {endpoint}");
        }
        for endpoint in ["/api/logs?limit=1", "/api/ping"] {
            let result = client.get(format!("http://{addr}{endpoint}")).header("Cookie", format!("{}={}", f50_server::clients::COOKIE, browser.id)).send().await.unwrap();
            assert!(result.status().is_success(), "pages still use {endpoint}: {}", result.status());
        }
        task.abort();
    }
    #[tokio::test]
    async fn display_change_during_phone_switch_does_not_reinvite_the_old_session() {
        let (manager,_server,hub,dir)=crate::bluetooth::recovery_tests::admin_fixture().await;
        let a=Admin::new(manager,hub.clone(),"https://test.invalid".into(),dir.path().join("cowplay.uci"),dir.path().join("config"),dir.path().to_owned(),"cert".into(),"key".into(),"127.0.0.1:8080".parse().unwrap(),"127.0.0.1:7000".parse().unwrap(),None,(800,480)).expect("valid synthetic policy");
        assert!(dir.path().join("cowplay.uci").exists(), "first start writes the UCI policy");
        hub.phone_active.store(true,std::sync::atomic::Ordering::Release);hub.pause_phone().unwrap();
        a.reinvite("display changed");
        assert!(!a.reinviting.load(std::sync::atomic::Ordering::Acquire),"an old session cannot schedule a reinvite under the new selection epoch");
        assert!(!hub.take_planned_restart());
    }
    #[test]
    fn configured_logging_never_claims_unopened_or_unsynced_storage_is_ok() {
        assert_eq!(log_storage_check(true,&json!({"active":false,"failures":0})).0,"fail");
        assert_eq!(log_storage_check(true,&json!({"active":true,"failures":0,"last_sync_ms":0})).0,"warn");
        assert_eq!(log_storage_check(true,&json!({"active":true,"failures":0,"last_sync_ms":123,"sync_age_ms":10,"queue_dropped":0})).0,"ok");
        assert_eq!(log_storage_check(true,&json!({"active":false,"failures":1,"error":"synthetic unavailable"})).0,"fail");
        assert_eq!(log_storage_check(true,&json!({"active":true,"failures":0,"last_sync_ms":123,"sync_age_ms":30_001})).0,"warn");
    }
    #[test]
    fn sharp_uses_largest_fitting_standard_preset() {
        assert_eq!(best_preset((1920, 928)), Some((1920, 720)), "odd browser window snaps to a verified size");
        assert_eq!(best_preset((1920, 1080)), Some((1920, 1080)));
        assert_eq!(best_preset((1280, 800)), Some((1280, 720)));
        assert_eq!(best_preset((640, 400)), None);
    }
    #[test]
    fn prefs_validation() {
        let mut p = Prefs::default();
        assert!(p.validate().is_ok());
        p.ui.rail_side = "top".into();
        assert!(p.validate().is_err());
        assert!(serde_json::from_value::<Prefs>(json!({"ui": {"x": 1}})).is_err());
        let mut p = Prefs::default();
        p.watchdog.stall_ms = 50;
        assert!(p.validate().is_err());
        let mut p = Prefs::default();
        p.display.quality = "custom".into();
        p.display.width = 1024;
        p.display.height = 600;
        assert!(p.validate().is_ok(), "any even size within the implemented envelope");
        p.display.width=1025;assert!(p.validate().is_err(),"odd dimensions");
        p.display.width = 1920;
        p.display.height = 720;
        p.display.fps = 45;
        assert!(p.validate().is_ok(),"implemented 45 fps declaration");
        p.display.fps=47;assert!(p.validate().is_err(),"unsupported frame-rate step");
        p.display.fps = 60;
        assert!(p.validate().is_ok());
    }
    #[test]
    fn picture_region_is_negotiated_without_stretching() {
        assert_eq!(fit_box(1208.0, 720.0, 1920.0, 1080.0), Some((1208, 720)));
        assert_eq!(fit_box(1812.0, 1080.0, 1920.0, 1080.0), Some((1812, 1080)), "sharp = 1.5 × region");
        assert_eq!(fit_box(1812.0, 1080.0, 1280.0, 720.0), Some((1208, 720)), "720p cap keeps aspect");
        assert_eq!(fit_box(2772.0, 1080.0, 1920.0, 1080.0), Some((1920, 748)));
        assert_eq!(fit_box(100.0, 100.0, 1920.0, 1080.0), None);
    }
    #[tokio::test]
    async fn independent_picture_settings_reach_the_phone_declaration() {
        let (a, _server, _commands, _dir) = display_fixture().await;
        let parsed = serde_json::from_value::<f50_server::clients::ScreenPrefs>(json!({
            "quality":"custom", "resolution":"1440p", "fps":45, "codec":"hevc"
        }));
        assert!(parsed.is_ok(), "independent settings must be accepted: {parsed:?}");
        let prefs = parsed.unwrap();
        assert!(prefs.validate().is_ok());
        let window = f50_server::clients::Window {w:1920,h:928,dpr:2.0,rw:1826,rh:928};
        let mut policy = crate::policy::Policy::default(); policy.max_res = "1440p".into();
        let spec = a.spec_for(&prefs, Some(window), &policy);
        assert_eq!((spec.width,spec.height,spec.fps,spec.hevc), (2560,1300,45,true));
        policy.max_res = "720p".into(); policy.max_fps = 30;
        let limited = a.spec_for(&prefs, Some(window), &policy);
        assert_eq!((limited.width,limited.height,limited.fps), (1280,650,30));
        policy.max_res="2160p".into();policy.max_fps=60;
        let mut p=prefs.clone();p.resolution="native".into();p.codec="h264".into();
        let native=a.spec_for(&p,Some(window),&policy);
        assert_eq!((native.width,native.height),(3652,1856),"DPR 2 can reach the physical screen pixels when explicitly allowed");
        p.resolution="720p".into();p.fps=30;
        let recovery=a.spec_for(&p,Some(window),&policy);
        assert_eq!((recovery.width,recovery.height,recovery.fps,recovery.hevc),(1280,650,30,false),"compatibility recovery must work under a 4K policy");
        let w=f50_server::clients::Window{w:1280,h:720,dpr:1.0,rw:1208,rh:720};
        p.quality="sharp".into();let sharp=a.spec_for(&p,Some(w),&policy);
        p.quality="custom".into();p.resolution="1.5x".into();p.fps=45;
        let tuned=a.spec_for(&p,Some(w),&policy);
        assert_eq!((tuned.width,tuned.height),(sharp.width,sharp.height),"only changing fps preserves sharp's 1.5x picture");
        assert_eq!(tuned.fps,45);
    }
    #[tokio::test]
    async fn small_and_tall_caps_never_fall_back_above_the_requested_size() {
        let (admin,_server,_commands,_dir)=display_fixture().await;
        let window=f50_server::clients::Window{w:1280,h:720,rw:1208,rh:720,dpr:1.0};
        for value in ["320x240","320x1080","1920x240"] {
            let mut policy=admin.policy();policy.max_res=value.into();
            let (w,h)=policy.max_box();let spec=admin.spec_for(&policy.defaults,Some(window),&policy);
            assert!(spec.width as u32<=w&&spec.height as u32<=h,"{value}: {spec:?}");
            assert!(spec.width>=f50_core::info::MIN_WIDTH&&spec.height>=f50_core::info::MIN_HEIGHT);
        }
        assert_eq!(crate::console::display_limits(&admin)["iphone"]["ranges_known"],false);
        assert!(crate::console::display_limits(&admin)["observed"].is_null(),"requested dimensions must never masquerade as observed media");
    }
    #[test]
    fn car_window_fits_receiver_envelope() {
        assert_eq!(fit_window(1280.0, 720.0), Some((1280, 720)));
        assert_eq!(fit_window(2560.0, 1440.0), Some((1920, 1080)));
        assert_eq!(fit_window(3840.0, 1200.0), Some((1920, 600)));
        assert_eq!(fit_window(1601.0, 901.0), Some((1600, 900)));
        assert_eq!(fit_window(200.0, 900.0), None);
    }
    #[test]
    fn legacy_hevc_setting_does_not_discard_other_configuration() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("ui-settings.json");
        let mut prefs=Prefs::default();prefs.display.codec="hevc".into();
        prefs.ui.rail_side="right".into();prefs.ddns.enabled=true;
        prefs.ddns.domain="car.example.test".into();prefs.log.syslog=true;
        assert!(prefs.validate().is_err(),"new writes must reject an inert codec");
        std::fs::write(&path,serde_json::to_vec(&prefs).unwrap()).unwrap();
        let migrated=legacy_policy(&path);
        assert_eq!(migrated.defaults.rail_side,"right");
        assert!(migrated.ddns_enabled&&migrated.log_syslog);
        assert_eq!(migrated.domain,"car.example.test");
    }
    #[test]
    fn external_log_dir_rejects_flash_and_traversal() {
        for bad in ["relative/x", "/tmp/x", "/mnt", "/mnt/sda1/../etc", "/mnt/./sda1/x"] {
            assert!(external_log_dir(bad, false).is_err(), "{bad}");
        }
    }
    #[test]
    fn redaction_hides_tokens_and_passwords_but_keeps_paths() {
        let v = redact(json!({"web": {"access_token": "123456", "origin": "https://x"}, "wifi": {"password_file": "/p", "password": "s"}}));
        assert_eq!(v["web"]["access_token"], "••••••••");
        assert_eq!(v["wifi"]["password"], "••••••••");
        assert_eq!(v["wifi"]["password_file"], "/p");
        assert_eq!(v["web"]["origin"], "https://x");
    }
    #[test]
    fn tesla_config_redaction_never_exports_vin_or_private_key_path(){
        let v=redact(json!({"tesla_ble":{"vin":"5YJ30123456789ABC","private_key":"/synthetic/private/tesla.pk8"}}));
        let mut rendered=String::new();render_toml(&mut rendered,"",&v);
        assert!(!rendered.contains("5YJ30123456789ABC"));assert!(!rendered.contains("tesla.pk8"));
        assert_eq!(v["tesla_ble"]["configured"],true);
    }
    #[test]
    fn certificate_not_after_from_complete_x509_and_rejects_invalid_der() {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["cowplay.test".into()]).unwrap();
        params.not_after = rcgen::date_time_ymd(2027, 3, 15);
        let cert = params.self_signed(&key).unwrap();
        assert_eq!(cert_not_after(cert.der()), Some(1_805_068_800));
        assert!(cert_not_after(&[0x30, 0]).is_none());
        assert!(cert_not_after(&cert.der()[..cert.der().len() - 1]).is_none());
    }
}
