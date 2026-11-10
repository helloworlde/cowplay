//! Authorized browsers ("clients"): car screens and phones.
//!
//! A browser becomes a client once: by entering the 6-digit access token (car keypad), by a
//! one-time ticket scanned from an authorized screen's QR code (phone), or automatically when
//! the administrator turned the token requirement off. The client id lives in an HttpOnly
//! cookie; every screen keeps its own display preferences on the server.
use crate::settings::write_private;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io::Read,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;

pub const COOKIE: &str = "cowplay_client";
const MAX_CLIENTS: usize = 64;
const MAX_FAILURES: usize = 5;
const LOCK: Duration = Duration::from_secs(600);
const TICKET_TTL: Duration = Duration::from_secs(300);

/// Per-screen preferences. The active screen's values drive the CarPlay session.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ScreenPrefs {
    /// auto | smooth | sharp | custom
    pub quality: String,
    /// native | 1.5x | 720p | 1080p | 1440p | 2160p; fitted to the screen aspect.
    pub resolution: String,
    /// Custom maxFPS, 30..=60 in steps of five. Actual input may be lower on a static screen.
    pub fps: u8,
    /// h264 | hevc (offer only; the phone chooses the actual codec).
    pub codec: String,
    /// bt | web
    pub call: String,
    /// left | right
    pub rail_side: String,
    /// small | std | large
    pub ui_size: String,
    /// CarPlay layout declaration; independent of the browser control rail.
    pub right_hand_drive: bool,
    pub night: bool,
    pub debug: bool,
}
impl Default for ScreenPrefs {
    fn default() -> Self {
        Self { quality: "auto".into(), resolution: "native".into(), fps: 60, codec: "h264".into(), call: "bt".into(), rail_side: "left".into(), ui_size: "std".into(), right_hand_drive: false, night: false, debug: false }
    }
}
impl ScreenPrefs {
    pub fn validate(&self) -> Result<(), &'static str> {
        let ok = matches!(self.quality.as_str(), "auto" | "smooth" | "sharp" | "custom")
            && (matches!(self.resolution.as_str(), "native" | "1.5x" | "720p" | "1080p" | "1440p" | "2160p") || self.exact_resolution().is_some())
            && (30..=60).contains(&self.fps) && self.fps % 5 == 0
            && matches!(self.codec.as_str(), "h264" | "hevc")
            && matches!(self.call.as_str(), "bt" | "web")
            && matches!(self.rail_side.as_str(), "left" | "right")
            && matches!(self.ui_size.as_str(), "small" | "std" | "large");
        if ok { Ok(()) } else { Err("invalid screen preference") }
    }
    pub fn exact_resolution(&self) -> Option<(u16, u16)> {
        let (w,h)=self.resolution.split_once('x')?;
        let (w,h)=(w.parse::<u16>().ok()?,h.parse::<u16>().ok()?);
        use f50_core::info::{MIN_WIDTH,MIN_HEIGHT,MAX_WIDTH,MAX_HEIGHT};
        ((MIN_WIDTH..=MAX_WIDTH).contains(&w)&&(MIN_HEIGHT..=MAX_HEIGHT).contains(&h)&&w%2==0&&h%2==0).then_some((w,h))
    }
}

/// Browser window as reported by the car page (CSS pixels) and the negotiated picture region.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub w: u32,
    pub h: u32,
    pub dpr: f64,
    /// Picture region in CSS pixels (window minus control rail, even).
    pub rw: u32,
    pub rh: u32,
}
impl Window {
    pub fn valid(&self) -> bool {
        (200..=8192).contains(&self.w)
            && (200..=8192).contains(&self.h)
            && self.dpr.is_finite()
            && (0.5..=4.0).contains(&self.dpr)
            && self.rw <= self.w
            && self.rh <= self.h
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    pub id: String,
    /// screen | phone
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub ua: String,
    #[serde(default)]
    pub ip: String,
    pub created_ms: u64,
    pub last_ms: u64,
    /// None = remembered until removed.
    pub expires_ms: Option<u64>,
    #[serde(default)]
    pub prefs: ScreenPrefs,
    #[serde(default)]
    pub window: Option<Window>,
}
impl Client {
    /// Short public handle (never the cookie value).
    pub fn handle(&self) -> String {
        self.id.chars().take(8).collect()
    }
    pub fn public(&self) -> serde_json::Value {
        serde_json::json!({"id":self.handle(),"kind":self.kind,"name":self.name,"ua":self.ua,"ip":self.ip,"created_ms":self.created_ms,"last_ms":self.last_ms,"expires_ms":self.expires_ms,"prefs":self.prefs,"window":self.window})
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub required: bool,
    /// None = forever.
    pub remember_days: Option<u32>,
}
impl Default for Policy {
    fn default() -> Self {
        Self { required: true, remember_days: None }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Denied {
    Wrong { remaining: usize },
    Locked { retry_after_s: u64 },
    Invalid,
}

struct Inner {
    dir: Option<PathBuf>,
    token: String,
    policy: Policy,
    defaults: ScreenPrefs,
    clients: Vec<Client>,
    failures: VecDeque<Instant>,
    locked_until: Option<Instant>,
    tickets: Vec<(String, Instant)>,
    /// Bumped on every change so pages can refresh lists.
    epoch: u64,
}

pub struct Clients {
    inner: Mutex<Inner>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
fn random_bytes<const N: usize>() -> std::io::Result<[u8; N]> {
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn valid_token(token: &str) -> bool {
    token.len() == 6 && token.bytes().all(|b| b.is_ascii_digit())
}
pub fn random_token() -> std::io::Result<String> {
    let b = random_bytes::<8>()?;
    Ok(format!("{:06}", u64::from_le_bytes(b) % 1_000_000))
}
fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
/// Value of the client cookie in a `Cookie:` header.
pub fn cookie_id(headers: &axum::http::HeaderMap) -> Option<String> {
    headers.get_all(axum::http::header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == COOKIE && v.len() == 32 && v.bytes().all(|b| b.is_ascii_hexdigit())).then(|| v.to_owned())
    })
}

impl Clients {
    /// In-memory store seeded with `token` when it is a 6-digit number.
    pub fn new(token: &str) -> Self {
        let token = if valid_token(token) { token.to_owned() } else { random_token().unwrap_or_else(|_| "000000".into()) };
        Self {
            inner: Mutex::new(Inner {
                dir: None,
                token,
                policy: Policy::default(),
                defaults: ScreenPrefs::default(),
                clients: Vec::new(),
                failures: VecDeque::new(),
                locked_until: None,
                tickets: Vec::new(),
                epoch: 0,
            }),
        }
    }
    /// Persist under `dir` (clients.json, access-token). An existing token file wins over the seed.
    pub fn load(&self, dir: PathBuf) -> std::io::Result<()> {
        let mut i = self.inner.lock().unwrap();
        match std::fs::read_to_string(dir.join("access-token")) {
            Ok(t) if valid_token(t.trim()) => i.token = t.trim().to_owned(),
            _ => write_private(&dir.join("access-token"), i.token.as_bytes())?,
        }
        if let Ok(bytes) = std::fs::read(dir.join("clients.json")) {
            if let Ok(mut list) = serde_json::from_slice::<Vec<Client>>(&bytes) {
                list.retain(|c| c.id.len() == 32 && c.prefs.validate().is_ok());
                list.truncate(MAX_CLIENTS);
                i.clients = list;
            }
        }
        i.dir = Some(dir);
        Ok(())
    }
    pub fn set_policy(&self, policy: Policy, defaults: ScreenPrefs) {
        let mut i = self.inner.lock().unwrap();
        i.policy = policy;
        if defaults.validate().is_ok() {
            i.defaults = defaults;
        }
        i.epoch += 1;
    }
    pub fn policy(&self) -> Policy {
        self.inner.lock().unwrap().policy
    }
    pub fn epoch(&self) -> u64 {
        self.inner.lock().unwrap().epoch
    }
    pub fn token(&self) -> String {
        self.inner.lock().unwrap().token.clone()
    }
    /// New random token. Existing clients stay authorized (remove them separately).
    pub fn reset_token(&self) -> std::io::Result<String> {
        let mut i = self.inner.lock().unwrap();
        let token = random_token()?;
        if let Some(dir) = &i.dir {
            write_private(&dir.join("access-token"), token.as_bytes())?;
        }
        i.token = token.clone();
        i.failures.clear();
        i.locked_until = None;
        i.epoch += 1;
        Ok(token)
    }
    /// Seconds left in a lockout, if any.
    pub fn locked_s(&self) -> Option<u64> {
        let mut i = self.inner.lock().unwrap();
        lock_left(&mut i)
    }
    /// The authorized, unexpired client for this cookie id.
    pub fn get(&self, id: &str) -> Option<Client> {
        let mut i = self.inner.lock().unwrap();
        let now = now_ms();
        let pos = i.clients.iter().position(|c| ct_eq(&c.id, id))?;
        if i.clients[pos].expires_ms.is_some_and(|e| e <= now) {
            i.clients.remove(pos);
            i.epoch += 1;
            let _ = persist(&i);
            return None;
        }
        // Touch at most once a minute to spare flash.
        if now.saturating_sub(i.clients[pos].last_ms) > 60_000 {
            i.clients[pos].last_ms = now;
            let _ = persist(&i);
        }
        Some(i.clients[pos].clone())
    }
    pub fn from_headers(&self, headers: &axum::http::HeaderMap) -> Option<Client> {
        self.get(&cookie_id(headers)?)
    }
    pub fn list(&self) -> Vec<Client> {
        let now = now_ms();
        self.inner.lock().unwrap().clients.iter().filter(|c| c.expires_ms.is_none_or(|e| e > now)).cloned().collect()
    }
    /// Verify a token attempt and admit a new client.
    pub fn admit_token(&self, token: &str, kind: &str, name: &str, ua: &str, ip: &str) -> Result<Client, Denied> {
        let mut i = self.inner.lock().unwrap();
        if let Some(retry_after_s) = lock_left(&mut i) {
            return Err(Denied::Locked { retry_after_s });
        }
        if !valid_token(token) || !ct_eq(token, &i.token) {
            let now = Instant::now();
            i.failures.push_back(now);
            while i.failures.front().is_some_and(|t| now.duration_since(*t) > LOCK) {
                i.failures.pop_front();
            }
            if i.failures.len() >= MAX_FAILURES {
                i.locked_until = Some(now + LOCK);
                i.failures.clear();
                return Err(Denied::Locked { retry_after_s: LOCK.as_secs() });
            }
            return Err(Denied::Wrong { remaining: MAX_FAILURES - i.failures.len() });
        }
        i.failures.clear();
        admit(&mut i, kind, name, ua, ip).map_err(|_| Denied::Invalid)
    }
    /// Admit without a token when the administrator disabled the requirement.
    pub fn admit_open(&self, kind: &str, name: &str, ua: &str, ip: &str) -> Option<Client> {
        let mut i = self.inner.lock().unwrap();
        if i.policy.required {
            return None;
        }
        admit(&mut i, kind, name, ua, ip).ok()
    }
    /// One-time ticket for a QR code shown on an authorized screen (5 minutes).
    pub fn ticket(&self) -> std::io::Result<String> {
        let t = hex(&random_bytes::<16>()?);
        let mut i = self.inner.lock().unwrap();
        let now = Instant::now();
        i.tickets.retain(|(_, at)| now.duration_since(*at) < TICKET_TTL);
        if i.tickets.len() >= 8 {
            i.tickets.remove(0);
        }
        i.tickets.push((t.clone(), now));
        Ok(t)
    }
    pub fn admit_ticket(&self, ticket: &str, kind: &str, name: &str, ua: &str, ip: &str) -> Option<Client> {
        let mut i = self.inner.lock().unwrap();
        let now = Instant::now();
        i.tickets.retain(|(_, at)| now.duration_since(*at) < TICKET_TTL);
        let pos = i.tickets.iter().position(|(t, _)| ct_eq(t, ticket))?;
        i.tickets.remove(pos);
        admit(&mut i, kind, name, ua, ip).ok()
    }
    pub fn set_prefs(&self, id: &str, prefs: ScreenPrefs) -> Result<Client, &'static str> {
        prefs.validate()?;
        let mut i = self.inner.lock().unwrap();
        let c = i.clients.iter_mut().find(|c| ct_eq(&c.id, id)).ok_or("unknown client")?;
        let before = c.clone();
        c.prefs = prefs;
        let out = c.clone();
        if persist(&i).is_err() {
            *i.clients.iter_mut().find(|c| ct_eq(&c.id, id)).unwrap() = before;
            return Err("无法保存屏幕设置，请检查设备存储后重试");
        }
        i.epoch += 1;
        Ok(out)
    }
    pub fn set_window(&self, id: &str, window: Window) {
        if !window.valid() {
            return;
        }
        let mut i = self.inner.lock().unwrap();
        if let Some(c) = i.clients.iter_mut().find(|c| ct_eq(&c.id, id)) {
            if c.window != Some(window) {
                c.window = Some(window);
                i.epoch += 1;
                let _ = persist(&i);
            }
        }
    }
    /// Remove by public handle; returns whether one was removed.
    pub fn remove(&self, handle: &str) -> std::io::Result<bool> {
        let mut i = self.inner.lock().unwrap();
        let before = i.clients.clone();
        i.clients.retain(|c| c.handle() != handle);
        let removed = i.clients.len() != before.len();
        if removed {
            if let Err(e) = persist(&i) { i.clients = before; return Err(e); }
            i.epoch += 1;
        }
        Ok(removed)
    }
    pub fn cookie(&self, client: &Client) -> String {
        let max_age = client.expires_ms.map(|e| e.saturating_sub(now_ms()) / 1000).unwrap_or(10 * 365 * 86_400);
        format!("{COOKIE}={}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Strict", client.id)
    }
}
fn lock_left(i: &mut Inner) -> Option<u64> {
    match i.locked_until {
        Some(t) if t > Instant::now() => Some(t.duration_since(Instant::now()).as_secs().max(1)),
        Some(_) => {
            i.locked_until = None;
            None
        }
        None => None,
    }
}
fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}
fn admit(i: &mut Inner, kind: &str, name: &str, ua: &str, ip: &str) -> std::io::Result<Client> {
    if !matches!(kind, "screen" | "phone") {
        return Err(std::io::Error::other("kind"));
    }
    let now = now_ms();
    let client = Client {
        id: hex(&random_bytes::<16>()?),
        kind: kind.into(),
        name: clean(name, 48),
        ua: clean(ua, 200),
        ip: clean(ip, 64),
        created_ms: now,
        last_ms: now,
        expires_ms: i.policy.remember_days.map(|d| now + d as u64 * 86_400_000),
        prefs: i.defaults.clone(),
        window: None,
    };
    let before = i.clients.clone();
    i.clients.retain(|c| c.expires_ms.is_none_or(|e| e > now));
    if i.clients.len() >= MAX_CLIENTS {
        // Drop the least recently used client.
        if let Some(pos) = i.clients.iter().enumerate().min_by_key(|(_, c)| c.last_ms).map(|(p, _)| p) {
            i.clients.remove(pos);
        }
    }
    i.clients.push(client.clone());
    if let Err(e) = persist(i) { i.clients = before; return Err(e); }
    i.epoch += 1;
    Ok(client)
}
fn persist(i: &Inner) -> std::io::Result<()> {
    let Some(dir) = &i.dir else { return Ok(()) };
    let bytes = serde_json::to_vec(&i.clients)?;
    write_private(&dir.join("clients.json"), &bytes).inspect_err(|e| {
        crate::diag_log!(Warn, "web", "clients store write failed: {e}");
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_lockout_after_five_failures() {
        let c = Clients::new("123456");
        for left in (1..5).rev() {
            assert_eq!(c.admit_token("000000", "screen", "", "", "").unwrap_err(), Denied::Wrong { remaining: left });
        }
        assert!(matches!(c.admit_token("000000", "screen", "", "", ""), Err(Denied::Locked { .. })));
        assert!(matches!(c.admit_token("123456", "screen", "", "", ""), Err(Denied::Locked { .. })), "even the right token waits");
        c.inner.lock().unwrap().locked_until = Some(Instant::now());
        let client = c.admit_token("123456", "screen", "Tesla", "ua", "1.2.3.4").unwrap();
        assert_eq!(c.get(&client.id).unwrap().name, "Tesla");
        assert!(c.get(&"0".repeat(32)).is_none());
    }
    #[test]
    fn tickets_are_single_use_and_open_policy_admits() {
        let c = Clients::new("123456");
        let t = c.ticket().unwrap();
        assert!(c.admit_ticket(&t, "phone", "", "", "").is_some());
        assert!(c.admit_ticket(&t, "phone", "", "", "").is_none());
        assert!(c.admit_open("screen", "", "", "").is_none());
        c.set_policy(Policy { required: false, remember_days: Some(30) }, ScreenPrefs::default());
        let s = c.admit_open("screen", "", "", "").unwrap();
        assert!(s.expires_ms.unwrap() > now_ms() + 29 * 86_400_000);
        assert!(c.cookie(&s).contains("HttpOnly; Secure; SameSite=Strict"));
    }
    #[test]
    fn prefs_and_persistence_round_trip() {
        let dir = tempdir();
        let c = Clients::new("not-a-number");
        c.load(dir.clone()).unwrap();
        let token = c.token();
        assert!(valid_token(&token));
        let s = c.admit_token(&token, "screen", "car", "", "").unwrap();
        let mut p = ScreenPrefs::default();
        p.quality = "custom".into();p.resolution="1440p".into();p.fps=45;p.codec="hevc".into();
        c.set_prefs(&s.id, p.clone()).unwrap();
        p.quality = "ultra".into();
        assert!(c.set_prefs(&s.id, p).is_err());
        let again = Clients::new("654321");
        again.load(dir.clone()).unwrap();
        assert_eq!(again.token(), token, "stored token wins over the seed");
        let saved=again.get(&s.id).unwrap().prefs;
        assert_eq!((saved.quality.as_str(),saved.resolution.as_str(),saved.fps,saved.codec.as_str()),("custom","1440p",45,"hevc"));
        assert!(again.remove(&s.handle()).unwrap());
        assert!(again.get(&s.id).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn old_screen_settings_load_with_compatible_independent_defaults() {
        let prefs: ScreenPrefs=serde_json::from_str(r#"{"quality":"smooth","call":"bt","rail_side":"left","ui_size":"std","night":false,"debug":false}"#).unwrap();
        assert_eq!((prefs.resolution.as_str(),prefs.fps,prefs.codec.as_str()),("native",60,"h264"));
        assert!(prefs.validate().is_ok());
        for patch in [serde_json::json!({"fps":33}),serde_json::json!({"fps":120}),serde_json::json!({"codec":"av1"}),serde_json::json!({"resolution":"8k"})] {
            let p:ScreenPrefs=serde_json::from_value(patch).unwrap();assert!(p.validate().is_err());
        }
    }
    #[test]
    fn cookie_parsing_is_strict() {
        let mut h = axum::http::HeaderMap::new();
        h.insert("cookie", format!("a=b; {COOKIE}={}", "ab".repeat(16)).parse().unwrap());
        assert_eq!(cookie_id(&h).unwrap(), "ab".repeat(16));
        h.insert("cookie", format!("{COOKIE}=../../x").parse().unwrap());
        assert!(cookie_id(&h).is_none());
    }
    #[test]
    fn failed_storage_does_not_publish_preferences_revocation_or_admission() {
        let dir = tempdir();
        let c = Clients::new("123456");
        c.load(dir.clone()).unwrap();
        let client = c.admit_token("123456", "screen", "car", "", "").unwrap();
        std::fs::remove_file(dir.join("clients.json")).unwrap();
        std::fs::create_dir(dir.join("clients.json")).unwrap();
        let mut p = client.prefs.clone(); p.quality = "sharp".into();
        assert!(c.set_prefs(&client.id, p).is_err());
        assert_eq!(c.get(&client.id).unwrap().prefs, client.prefs);
        assert!(c.remove(&client.handle()).is_err());
        assert!(c.get(&client.id).is_some());
        assert!(c.admit_token("123456", "phone", "", "", "").is_err());
        assert_eq!(c.list().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
    fn tempdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("cowplay-clients-{}-{}", std::process::id(), hex(&random_bytes::<4>().unwrap())));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}

#[cfg(test)]
mod display_alignment_tests {
    use super::*;
    #[test]
    fn exact_sizes_are_bounded_and_old_preferences_keep_left_hand_drive() {
        let mut p:ScreenPrefs=serde_json::from_str(r#"{"quality":"custom","resolution":"720x1280"}"#).unwrap();
        assert!(!p.right_hand_drive);assert!(p.validate().is_ok());assert_eq!(p.exact_resolution(),Some((720,1280)));
        for value in ["0x0","3842x2160","720x2162","721x1280","NaNx720","65536x720"] {p.resolution=value.into();assert!(p.validate().is_err(),"{value}");}
    }
}
