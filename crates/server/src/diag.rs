//! In-memory observability shared by the bridge and the platform service: a bounded log ring,
//! per-session timelines and one-second metric samples. Nothing here leaves the device unless an
//! authenticated user downloads it; optional sinks (stderr mirror, external log file) are opt-in.
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, serde::Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}
impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub seq: u64,
    pub ts_ms: u64,
    pub level: Level,
    pub module: &'static str,
    pub msg: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub ts_ms: u64,
    /// ok | warn | info | error
    pub kind: &'static str,
    pub msg: String,
    /// Cause or extra line shown under the title.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Session {
    pub id: String,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    pub phone: Option<String>,
    pub peer: Option<String>,
    pub end_reason: Option<String>,
    pub stalls: u32,
    pub warnings: u32,
    pub first_frame_ms: Option<u64>,
    /// Hardware identifier (e.g. iPhone14,2) and iOS version the iPhone sent in its CarPlay SETUP.
    pub model: Option<String>,
    pub os: Option<String>,
    pub events: Vec<Event>,
}

/// Where the phone connection currently stands, step by step, for the car/phone/admin pages.
/// Each step is `idle | active | ok | fail`; `error` names the failing step in plain language.
#[derive(Clone, Debug, Serialize)]
pub struct Link {
    pub bluetooth: &'static str,
    pub bluetooth_detail: String,
    pub wifi: &'static str,
    pub wifi_detail: String,
    pub carplay: &'static str,
    pub carplay_detail: String,
    pub error: Option<LinkError>,
    pub updated_ms: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct LinkError {
    /// bluetooth | wifi | carplay
    pub step: &'static str,
    pub title: String,
    pub hint: String,
    pub ts_ms: u64,
}
impl Default for Link {
    fn default() -> Self {
        Self {
            bluetooth: "idle",
            bluetooth_detail: "未连接".into(),
            wifi: "idle",
            wifi_detail: "等待蓝牙下发热点信息".into(),
            carplay: "idle",
            carplay_detail: "等待 iPhone 连接".into(),
            error: None,
            updated_ms: 0,
        }
    }
}

/// Metrics reported by the car browser over the authenticated viewer channel.
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewerPing {
    pub seq: u64,
    pub rtt_ms: f64,
    pub age_ms: u64,
}
#[derive(Clone, Debug, Default, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewerReport {
    pub fps: Option<f64>,
    pub decode_ms: Option<f64>,
    pub rtt_ms: Option<f64>,
    /// Latest individual pong, separate from the rolling diagnostic averages.
    pub ping: Option<ViewerPing>,
    pub decoded: Option<u64>,
    pub dropped: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub decoder: Option<String>,
    pub intervals: Option<Vec<f64>>,
    pub audio_ms: Option<f64>,
    /// Car ⟷ F50 ping jitter and lost pings over the last minute.
    pub jitter_ms: Option<f64>,
    pub loss_pct: Option<f64>,
    /// Decoder queue depth expressed as time.
    pub buffer_ms: Option<f64>,
    pub audio_rate: Option<u32>,
    pub audio_channels: Option<u8>,
    pub audio_codec: Option<String>,
    pub codec: Option<String>,
    pub color: Option<String>,
    pub visible: Option<bool>,
}

const LOG_CAP_BYTES: usize = 4 * 1024 * 1024;
const MSG_LIMIT: usize = 1024;
const HISTORY: usize = 60;
const SESSION_EVENTS: usize = 200;
const SESSION_HISTORY: usize = 5;
const VERBOSE_FOR: Duration = Duration::from_secs(30 * 60);

struct Logs {
    entries: VecDeque<Entry>,
    bytes: usize,
    seq: u64,
    evicted: u64,
}
struct Sessions {
    current: Option<Session>,
    history: VecDeque<Session>,
}
/// External log sink. Lines go through a bounded channel to a dedicated thread so a stalled
/// USB device can never block callers of `record()`; lines are dropped when the queue is full.
struct Sink {
    tx: std::sync::mpsc::SyncSender<String>,
    path: std::path::PathBuf,
    file: Arc<SinkFile>,
}
#[derive(Default)]
struct SinkState {
    queued: AtomicU64,
    written: AtomicU64,
    dropped: AtomicU64,
    failures: AtomicU64,
    last_sync_ms: AtomicU64,
    last_sync_at: Mutex<Option<Instant>>,
    error: Mutex<Option<String>>,
}
impl SinkState {
    fn synced(&self) {
        self.last_sync_ms.store(now_ms(), Ordering::Relaxed);
        *self.last_sync_at.lock().unwrap() = Some(Instant::now());
    }
    fn failed(&self, error: impl ToString) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        let error = mask_macs(&error.to_string());
        *self.error.lock().unwrap() = Some(error.clone());
        eprintln!("WARN CowPlay persistent log: {error}");
    }
}
struct SinkFile {
    log: Mutex<crate::log_store::LogFile>,
    keep_days: AtomicU8,
    state: Arc<SinkState>,
    device: u64,
    inode: Arc<AtomicU64>,
    rotating: Arc<AtomicBool>,
}
fn same_log_file(file: &SinkFile, opened: Option<&std::fs::File>, path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let Some(opened)=opened else {return Ok(());};
    let supplied=opened.metadata()?;
    let identity=(file.device,file.inode.load(Ordering::Acquire));
    if (supplied.dev(),supplied.ino())==identity {return Ok(());}
    if file.rotating.load(Ordering::Acquire) {return Ok(());}
    // The owned writer may have rotated since the caller opened its handle.
    let current=std::fs::symlink_metadata(path)?;
    if file.rotating.load(Ordering::Acquire) {return Ok(());}
    if current.is_file()&&(current.dev(),current.ino())==(file.device,file.inode.load(Ordering::Acquire)) {return Ok(());}
    Err(std::io::Error::other("日志文件已被替换，已停止旧写入；请重新配置日志目录"))
}
fn sink_thread(file: Arc<SinkFile>, rx: std::sync::mpsc::Receiver<String>) {
    let mut dirty = false;
    let mut synced = Instant::now();
    loop {
        let wait = Duration::from_secs(1).saturating_sub(synced.elapsed());
        match rx.recv_timeout(wait) {
            Ok(line) => {
                let result = {
                    let mut log = file.log.lock().unwrap();
                    let result = log.write(&line, now_ms() / 86_400_000, file.keep_days.load(Ordering::Relaxed));
                    result
                };
                if let Err(error) = result { file.state.failed(error); return; }
                file.state.written.fetch_add(1, Ordering::Relaxed);
                dirty = true;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if dirty {
                    match file.log.lock().unwrap().sync() {
                        Ok(()) => { file.state.synced(); }
                        Err(error) => file.state.failed(error),
                    }
                }
                return;
            }
        }
        if dirty && synced.elapsed() >= Duration::from_secs(1) {
            match file.log.lock().unwrap().sync() {
                Ok(()) => { file.state.synced(); dirty = false; }
                Err(error) => { file.state.failed(error); return; }
            }
        }
        if !dirty { synced = Instant::now(); }
    }
}
#[derive(Default)]
struct Metrics {
    frames_in: u64,
    bytes_in: u64,
    dropped: u64,
    keyframes: u64,
    last_frame: Option<Instant>,
    prev: (u64, u64, Option<Instant>),
    fps_in: f64,
    bitrate_bps: f64,
    phone_rtt_us: Option<u64>,
    phone_rtt_at: Option<Instant>,
    viewer: Option<(Instant, ViewerReport)>,
    viewer_owner: Option<(String, u64)>,
    caps: Option<serde_json::Value>,
    system: serde_json::Value,
    system_at: Option<Instant>,
    history: BTreeMap<&'static str, VecDeque<Option<f64>>>,
}

pub struct Diag {
    logs: Mutex<Logs>,
    sessions: Mutex<Sessions>,
    metrics: Mutex<Metrics>,
    verbose_until: Mutex<Option<Instant>>,
    mirror: AtomicBool,
    sink: Mutex<Option<Sink>>,
    sink_state: Mutex<Option<Arc<SinkState>>>,
    sink_drains: Mutex<Vec<tokio::sync::oneshot::Receiver<()>>>,
    sink_files: Mutex<BTreeMap<std::path::PathBuf, std::sync::Weak<SinkFile>>>,
    pub started: Instant,
    pub started_ms: u64,
    service_id: String,
    counter: AtomicU64,
    link: Mutex<Link>,
    recent: Mutex<VecDeque<Event>>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Unpredictable-enough short identifiers (not a secret; used for display only).
fn short_id(seed: u64) -> String {
    let mut bytes = [0u8; 2];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_ok();
    let value = if ok {
        u16::from_le_bytes(bytes) as u64
    } else {
        (now_ms() ^ seed.rotate_left(17)) & 0xffff
    };
    format!("{value:04x}")
}

pub fn diag() -> &'static Diag {
    static D: OnceLock<Diag> = OnceLock::new();
    D.get_or_init(Diag::new)
}

impl Diag {
    fn new() -> Self { Self {
        logs: Mutex::new(Logs {
            entries: VecDeque::new(),
            bytes: 0,
            seq: 0,
            evicted: 0,
        }),
        sessions: Mutex::new(Sessions {
            current: None,
            history: VecDeque::new(),
        }),
        metrics: Mutex::new(Metrics::default()),
        verbose_until: Mutex::new(None),
        mirror: AtomicBool::new(false),
        sink: Mutex::new(None),
        sink_state: Mutex::new(None),
        sink_drains: Mutex::new(Vec::new()),
        sink_files: Mutex::new(BTreeMap::new()),
        started: Instant::now(),
        started_ms: now_ms(),
        service_id: short_id(1),
        counter: AtomicU64::new(0),
        link: Mutex::new(Link::default()),
        recent: Mutex::new(VecDeque::new()),
    } }
}

/// Record a bounded log line. Debug output is opt-in for every destination.
pub fn record(level: Level, module: &'static str, msg: String, echo: bool) {
    record_for(diag(), level, module, msg, echo);
}

fn record_for(d: &Diag, level: Level, module: &'static str, msg: String, echo: bool) {
    let quiet_debug = level == Level::Debug && !d.verbose();
    if quiet_debug {
        return;
    }
    let mut msg = mask_macs(&msg);
    if msg.len() > MSG_LIMIT {
        let mut cut = MSG_LIMIT;
        while !msg.is_char_boundary(cut) {
            cut -= 1;
        }
        msg.truncate(cut);
        msg.push('…');
        // The log ring accounts retained bytes, not the original formatting buffer.
        msg.shrink_to_fit();
    }
    let ts_ms = now_ms();
    if echo || d.mirror.load(Ordering::Relaxed) {
        eprintln!("{} {module}: {msg}", level.as_str());
    }
    let mut sink = d.sink.lock().unwrap();
    let line = {
        let mut logs = d.logs.lock().unwrap();
        logs.seq += 1;
        let entry = Entry {
            seq: logs.seq,
            ts_ms,
            level,
            module,
            msg,
        };
        let size = entry.msg.len() + 64;
        logs.bytes += size;
        while logs.bytes > LOG_CAP_BYTES {
            match logs.entries.pop_front() {
                Some(old) => { logs.bytes -= old.msg.len() + 64; logs.evicted += 1; },
                None => break,
            }
        }
        let line = sink.as_ref().map(|_| format_line(&entry));
        logs.entries.push_back(entry);
        line
    };
    if let (Some(s), Some(line)) = (sink.as_ref(), line) {
        match s.tx.try_send(line) {
            Ok(()) => { s.file.state.queued.fetch_add(1, Ordering::Relaxed); }
            Err(std::sync::mpsc::TrySendError::Full(_)) => { s.file.state.dropped.fetch_add(1, Ordering::Relaxed); }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                s.file.state.dropped.fetch_add(1, Ordering::Relaxed);
                s.file.state.failed("日志写入线程已停止，内存日志仍可导出");
                *sink = None;
            }
        }
    }
}

pub fn format_line(e: &Entry) -> String {
    format!(
        "{} {:<5} {:<7} {}\n",
        clock_text(e.ts_ms, true),
        e.level.as_str(),
        e.module,
        e.msg
    )
}

/// UTC wall-clock text; the device has no reliable time zone database.
pub fn clock_text(ts_ms: u64, millis: bool) -> String {
    let s = ts_ms / 1000;
    let (h, m, sec) = ((s / 3600) % 24, (s / 60) % 60, s % 60);
    if millis {
        format!("{h:02}:{m:02}:{sec:02}.{:03}", ts_ms % 1000)
    } else {
        format!("{h:02}:{m:02}:{sec:02}")
    }
}

#[macro_export]
macro_rules! diag_log {
    ($level:ident, $module:expr, $($arg:tt)*) => {
        if $crate::diag::Level::$level != $crate::diag::Level::Debug || $crate::diag::diag().verbose() {
            $crate::diag::record($crate::diag::Level::$level, $module, format!($($arg)*), false)
        }
    };
}
/// Same as `diag_log!`, also written to procd/logread when the level is enabled.
#[macro_export]
macro_rules! diag_echo {
    ($level:ident, $module:expr, $($arg:tt)*) => {
        if $crate::diag::Level::$level != $crate::diag::Level::Debug || $crate::diag::diag().verbose() {
            $crate::diag::record($crate::diag::Level::$level, $module, format!($($arg)*), true)
        }
    };
}

#[derive(Default, serde::Deserialize)]
pub struct LogQuery {
    /// all | error | warn | info | debug (inclusive thresholds)
    pub level: Option<String>,
    pub module: Option<String>,
    pub q: Option<String>,
    pub after: Option<u64>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct LogPage {
    pub entries: Vec<Entry>,
    pub matched: usize,
    pub total: usize,
    pub bytes: usize,
    pub cap_bytes: usize,
    pub evicted: u64,
    pub oldest_seq: Option<u64>,
    pub oldest_ms: Option<u64>,
    pub last_seq: u64,
}

impl Diag {
    pub fn verbose(&self) -> bool {
        let mut v = self.verbose_until.lock().unwrap();
        match *v {
            Some(t) if Instant::now() < t => true,
            Some(_) => {
                *v = None;
                false
            }
            None => false,
        }
    }
    pub fn verbose_remaining(&self) -> Option<u64> {
        if !self.verbose() {
            return None;
        }
        self.verbose_until
            .lock()
            .unwrap()
            .map(|t| t.saturating_duration_since(Instant::now()).as_secs())
    }
    pub fn set_verbose(&self, on: bool) {
        *self.verbose_until.lock().unwrap() = on.then(|| Instant::now() + VERBOSE_FOR);
    }
    pub fn set_mirror(&self, on: bool) {
        self.mirror.store(on, Ordering::Relaxed);
    }
    pub fn mirror(&self) -> bool {
        self.mirror.load(Ordering::Relaxed)
    }
    /// Attach or detach an append-only file sink. The caller validates the location.
    pub fn set_sink(&self, path: Option<std::path::PathBuf>) -> std::io::Result<()> {
        self.set_sink_with_retention(path, 0)
    }
    /// Retain this writer's daily external archives; zero disables expiry.
    pub fn set_sink_with_retention(&self, path: Option<std::path::PathBuf>, keep_days: u8) -> std::io::Result<()> {
        let result = self.replace_sink(path, keep_days, None);
        if let Err(error) = &result { self.sink_failed(error); }
        result
    }
    /// Production callers open the file atomically with platform no-follow and
    /// nonblocking flags, then pass its owned handle here. No path is reopened.
    pub fn set_sink_file(&self, path: std::path::PathBuf, file: std::fs::File, keep_days: u8) -> std::io::Result<()> {
        if !file.metadata()?.is_file() { return Err(std::io::Error::other("日志目标不是普通文件")); }
        let result = self.replace_sink(Some(path), keep_days, Some(file));
        if let Err(error) = &result { self.sink_failed(error); }
        result
    }
    /// A requested persistent destination is unavailable. Detach any old path so
    /// configuration errors never masquerade as a successful new destination.
    pub fn sink_failed(&self, error: impl ToString) {
        self.sink.lock().unwrap().take();
        let mut state = self.sink_state.lock().unwrap();
        state.get_or_insert_with(|| Arc::new(SinkState::default())).failed(error);
    }
    fn replace_sink(&self, path: Option<std::path::PathBuf>, keep_days: u8, opened: Option<std::fs::File>) -> std::io::Result<()> {
        let mut sink = self.sink.lock().unwrap();
        match path {
            None => {
                *sink = None;
                Ok(())
            }
            Some(path) => {
                let name = path.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing log filename"))?;
                let path = path.parent().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing log directory"))?.canonicalize()?.join(name);
                if let Some(s) = sink.as_ref().filter(|s| s.path == path) {
                    same_log_file(&s.file, opened.as_ref(), &path)?;
                    s.file.keep_days.store(keep_days, Ordering::Relaxed);
                    return Ok(());
                }
                // A detached channel can still be draining. Reuse that file's writer so
                // queued old/new lines cannot rotate independently or write unlinked files.
                let mut files = self.sink_files.lock().unwrap();
                files.retain(|_, f| f.strong_count() > 0);
                let file = match files.get(&path).and_then(std::sync::Weak::upgrade) {
                    Some(file) => { same_log_file(&file, opened.as_ref(), &path)?; file },
                    None => {
                        use std::os::unix::fs::OpenOptionsExt;
                        let opened = match opened {
                            Some(file) => file,
                            None => {
                                // This convenience path is for trusted callers. The service
                                // supplies its securely opened handle via set_sink_file.
                                if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
                                    return Err(std::io::Error::other("日志目标不是普通文件"));
                                }
                                std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&path)?
                            }
                        };
                        let log = crate::log_store::LogFile::new(path.clone(), opened, now_ms() / 86_400_000)?;
                        let (device,_)=log.identity();
                        let (inode,rotating)=log.identity_state();
                        let file = Arc::new(SinkFile { log: Mutex::new(log), keep_days: AtomicU8::new(keep_days), state: Arc::new(SinkState::default()), device, inode, rotating });
                        files.insert(path.clone(), Arc::downgrade(&file));
                        file
                    }
                };
                file.keep_days.store(keep_days, Ordering::Relaxed);
                drop(files);
                let (tx, rx) = std::sync::mpsc::sync_channel(4096);
                let thread_file = file.clone();
                let (done, drained) = tokio::sync::oneshot::channel();
                std::thread::Builder::new()
                    .name("log-sink".into())
                    .spawn(move || { sink_thread(thread_file, rx); let _ = done.send(()); })?;
                let mut drains = self.sink_drains.lock().unwrap();
                drains.retain_mut(|r| matches!(r.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
                drains.push(drained);
                *self.sink_state.lock().unwrap() = Some(file.state.clone());
                *sink = Some(Sink { tx, path, file });
                Ok(())
            }
        }
    }
    /// Stop accepting file writes, then drain and sync all owned writers within the
    /// service's existing shutdown deadline. Never block the runtime on a disk join.
    pub async fn shutdown_sink(&self, deadline: tokio::time::Instant) -> bool {
        self.sink.lock().unwrap().take();
        let drains = std::mem::take(&mut *self.sink_drains.lock().unwrap());
        let mut drains = drains;
        while let Some(mut drain) = drains.pop() {
            if !matches!(tokio::time::timeout_at(deadline, &mut drain).await, Ok(Ok(()))) {
                drains.push(drain);
                self.sink_drains.lock().unwrap().extend(drains);
                if let Some(state) = self.sink_state.lock().unwrap().as_ref() { state.failed("日志排空/同步未在停服预算内完成"); }
                return false;
            }
        }
        self.sink_state.lock().unwrap().as_ref().is_none_or(|s| s.failures.load(Ordering::Relaxed) == 0)
    }
    pub fn storage_status(&self) -> serde_json::Value {
        let active = self.sink_path();
        let guard = self.sink_state.lock().unwrap();
        let Some(s) = guard.as_ref() else { return serde_json::json!({"active": false, "queued": 0, "written": 0, "queue_dropped": 0, "failures": 0}); };
        serde_json::json!({"active": active.is_some() && s.failures.load(Ordering::Relaxed)==0, "path": active,
            "queued": s.queued.load(Ordering::Relaxed), "written": s.written.load(Ordering::Relaxed),
            "queue_dropped": s.dropped.load(Ordering::Relaxed), "failures": s.failures.load(Ordering::Relaxed),
            "last_sync_ms": s.last_sync_ms.load(Ordering::Relaxed), "sync_age_ms":s.last_sync_at.lock().unwrap().map(|t|t.elapsed().as_millis() as u64), "error": *s.error.lock().unwrap(),
            "sync_interval_ms": 1000, "file_limit_bytes": 32*1024*1024, "archive_limit_bytes": 128*1024*1024})
    }
    pub fn sink_path(&self) -> Option<String> {
        self.sink
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.path.display().to_string())
    }

    pub fn logs(&self, q: &LogQuery) -> LogPage {
        let logs = self.logs.lock().unwrap();
        let threshold = match q.level.as_deref() {
            Some("error") => Level::Error,
            Some("warn") => Level::Warn,
            Some("info") => Level::Info,
            _ => Level::Debug,
        };
        let module = q.module.as_deref().filter(|m| !m.is_empty() && *m != "all");
        let needle = q
            .q
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase);
        let after = q.after.unwrap_or(0);
        let limit = q.limit.unwrap_or(500).clamp(1, 5000);
        let matched: Vec<&Entry> = logs
            .entries
            .iter()
            .filter(|e| e.seq > after && e.level <= threshold)
            .filter(|e| module.is_none_or(|m| e.module == m))
            .filter(|e| {
                needle
                    .as_ref()
                    .is_none_or(|n| e.msg.to_lowercase().contains(n) || e.module.contains(n.as_str()))
            })
            .collect();
        let count = matched.len();
        let entries = matched
            .into_iter()
            .rev()
            .take(limit)
            .cloned()
            .map(|mut entry| {entry.msg=mask_macs(&entry.msg);entry})
            .collect();
        LogPage {
            entries,
            matched: count,
            total: logs.entries.len(),
            bytes: logs.bytes,
            cap_bytes: LOG_CAP_BYTES,
            evicted: logs.evicted,
            oldest_seq: logs.entries.front().map(|e| e.seq),
            oldest_ms: logs.entries.front().map(|e| e.ts_ms),
            last_seq: logs.seq,
        }
    }
    pub fn warning_count(&self, since_ms: u64) -> usize {
        self.logs
            .lock()
            .unwrap()
            .entries
            .iter()
            .filter(|e| e.ts_ms >= since_ms && e.level <= Level::Warn)
            .count()
    }

    // ---- link steps ----
    pub fn link(&self) -> Link {
        self.link.lock().unwrap().clone()
    }
    /// Update one step. A new Bluetooth attempt clears earlier failures and later steps.
    pub fn link_step(&self, step: &'static str, state: &'static str, detail: impl Into<String>) {
        let mut l = self.link.lock().unwrap();
        let detail = detail.into();
        match step {
            "bluetooth" => {
                if state == "active" || (state == "ok" && l.bluetooth != "ok") {
                    let keep_carplay = l.carplay == "ok";
                    let fresh = Link::default();
                    l.error = None;
                    l.wifi = fresh.wifi;
                    l.wifi_detail = fresh.wifi_detail;
                    if !keep_carplay {
                        l.carplay = fresh.carplay;
                        l.carplay_detail = fresh.carplay_detail;
                    }
                }
                l.bluetooth = state;
                l.bluetooth_detail = detail;
            }
            "wifi" => {
                l.wifi = state;
                l.wifi_detail = detail;
            }
            _ => {
                if state == "active" && l.carplay != "active" {
                    l.error = None;
                }
                if state == "ok" {
                    // A streaming CarPlay session proves the earlier steps, even when the iPhone
                    // rejoined over Wi‑Fi on its own without a new Bluetooth hand-off.
                    l.error = None;
                    if l.bluetooth != "ok" {
                        l.bluetooth = "ok";
                        l.bluetooth_detail = "iPhone 已连接".into();
                    }
                    if l.wifi != "ok" {
                        l.wifi = "ok";
                        l.wifi_detail = "iPhone 已在 F50 热点".into();
                    }
                }
                l.carplay = state;
                l.carplay_detail = detail;
            }
        }
        l.updated_ms = now_ms();
    }
    /// Mark a step failed with a user-facing reason and next action; also logged as an error event.
    pub fn link_fail(&self, step: &'static str, title: impl Into<String>, hint: impl Into<String>) {
        let (title, hint) = (title.into(), hint.into());
        {
            let mut l = self.link.lock().unwrap();
            match step {
                "bluetooth" => {
                    l.bluetooth = "fail";
                    l.bluetooth_detail = title.clone();
                }
                "wifi" => {
                    l.wifi = "fail";
                    l.wifi_detail = title.clone();
                }
                _ => {
                    l.carplay = "fail";
                    l.carplay_detail = title.clone();
                }
            }
            l.error = Some(LinkError { step, title: title.clone(), hint, ts_ms: now_ms() });
            l.updated_ms = now_ms();
        }
        self.event("error", if step == "carplay" { "carplay" } else { "bt" }, title);
    }

    // ---- sessions ----
    pub fn session_begin(&self, source: &str) -> String {
        let id = {
            let mut s = self.sessions.lock().unwrap();
            if let Some(c) = s.current.as_ref() {
                return c.id.clone();
            }
            let n = self.counter.fetch_add(1, Ordering::Relaxed);
            let id = short_id(n);
            s.current = Some(Session {
                id: id.clone(),
                started_ms: now_ms(),
                ended_ms: None,
                phone: None,
                peer: None,
                end_reason: None,
                stalls: 0,
                warnings: 0,
                first_frame_ms: None,
                model: None,
                os: None,
                events: Vec::new(),
            });
            id
        };
        record(
            Level::Info,
            "carplay",
            format!("#{id} session begins ({source})"),
            false,
        );
        id
    }
    /// End a session that never reached a CarPlay control connection (e.g. Bluetooth-only).
    pub fn session_end_without_control(&self, reason: &str) {
        self.finish_session(reason, true);
    }
    pub fn session_end(&self, reason: &str) {
        self.finish_session(reason, false);
    }
    fn finish_session(&self, reason: &str, only_without_control: bool) {
        let ended = {
            let mut s = self.sessions.lock().unwrap();
            if only_without_control && s.current.as_ref().is_some_and(|c| c.peer.is_some()) {
                return;
            }
            self.metrics.lock().unwrap().phone_rtt_us = None;
            s.current.take().map(|mut c| {
                c.ended_ms = Some(now_ms());
                c.end_reason = Some(reason.to_owned());
                c.events.push(Event {
                    ts_ms: now_ms(),
                    kind: "info",
                    msg: format!("会话结束：{reason}"),
                    detail: String::new(),
                });
                let id = c.id.clone();
                s.history.push_front(c);
                s.history.truncate(SESSION_HISTORY);
                id
            })
        };
        if let Some(id) = ended {
            record(
                Level::Info,
                "carplay",
                format!("#{id} session ended: {reason}"),
                false,
            );
        }
    }
    pub fn session_id(&self) -> Option<String> {
        self.sessions
            .lock()
            .unwrap()
            .current
            .as_ref()
            .map(|c| c.id.clone())
    }
    /// Short code that a user can read out for support: session (or service) id + build tag.
    pub fn diag_code(&self) -> String {
        let id = self.session_id().unwrap_or_else(|| self.service_id.clone());
        let build = crate::build_info::BUILD_ID;
        let tag: String = build
            .chars()
            .rev()
            .filter(|c| c.is_ascii_hexdigit())
            .take(2)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("{}-{}", id.to_uppercase(), tag.to_uppercase())
    }
    pub fn set_peer(&self, peer: String, phone: Option<String>) {
        let mut s = self.sessions.lock().unwrap();
        if let Some(c) = s.current.as_mut() {
            c.peer = Some(peer);
            if phone.is_some() {
                c.phone = phone;
            }
        }
    }
    pub fn peer(&self) -> Option<String> {
        self.sessions
            .lock()
            .unwrap()
            .current
            .as_ref()
            .and_then(|c| c.peer.clone())
    }
    /// Only validated public strings (see the receiver) reach here.
    pub fn set_phone_info(&self, model: Option<String>, os: Option<String>) {
        if let Some(c) = self.sessions.lock().unwrap().current.as_mut() {
            if model.is_some() { c.model = model; }
            if os.is_some() { c.os = os; }
        }
    }
    pub fn set_phone(&self, phone: String) {
        if let Some(c) = self.sessions.lock().unwrap().current.as_mut() {
            c.phone = Some(phone);
        }
    }
    /// Timeline event. Also mirrored into the log ring under `module`.
    pub fn event(&self, kind: &'static str, module: &'static str, msg: impl Into<String>) {
        self.event_detail(kind, module, msg, String::new());
    }
    /// Recent events across sessions, newest first.
    pub fn recent(&self, n: usize) -> Vec<Event> {
        self.recent.lock().unwrap().iter().rev().take(n).cloned().collect()
    }
    /// Timeline event with a cause line.
    pub fn event_detail(&self, kind: &'static str, module: &'static str, msg: impl Into<String>, detail: impl Into<String>) {
        let msg = msg.into();
        let detail = detail.into();
        {
            let mut r = self.recent.lock().unwrap();
            r.push_back(Event { ts_ms: now_ms(), kind, msg: msg.clone(), detail: detail.clone() });
            while r.len() > 100 {
                r.pop_front();
            }
        }
        let id = {
            let mut s = self.sessions.lock().unwrap();
            s.current.as_mut().map(|c| {
                if kind == "warn" || kind == "error" {
                    c.warnings += 1;
                }
                if c.events.len() < SESSION_EVENTS {
                    c.events.push(Event {
                        ts_ms: now_ms(),
                        kind,
                        msg: msg.clone(),
                        detail: detail.clone(),
                    });
                }
                c.id.clone()
            })
        };
        let level = match kind {
            "error" => Level::Error,
            "warn" => Level::Warn,
            _ => Level::Info,
        };
        let prefix = id.map(|i| format!("#{i} ")).unwrap_or_default();
        let tail = if detail.is_empty() { String::new() } else { format!(" — {detail}") };
        record(level, module, format!("{prefix}{msg}{tail}"), false);
    }
    pub fn first_frame(&self) {
        let first = {
            let mut s = self.sessions.lock().unwrap();
            match s.current.as_mut() {
                Some(c) if c.first_frame_ms.is_none() => {
                    c.first_frame_ms = Some(now_ms());
                    true
                }
                _ => false,
            }
        };
        if first {
            self.event("ok", "web", "车机收到首帧并完成绘制");
        }
    }
    pub fn stall(&self, ms: u64) {
        if let Some(c) = self.sessions.lock().unwrap().current.as_mut() {
            c.stalls += 1;
        }
        self.event_detail("warn", "video", format!("画面停住 {:.1} 秒，已恢复", ms as f64 / 1000.0), "车机收到新帧后绘制未及时推进，现已恢复绘制");
    }
    pub fn sessions(&self) -> (Option<Session>, Vec<Session>) {
        let s = self.sessions.lock().unwrap();
        (s.current.clone(), s.history.iter().cloned().collect())
    }

    // ---- metrics ----
    pub fn frame_in(&self, bytes: usize, dropped: u64) {
        let mut m = self.metrics.lock().unwrap();
        m.frames_in += 1;
        m.bytes_in += bytes as u64;
        m.last_frame = Some(Instant::now());
        m.dropped += dropped;
    }
    pub fn keyframe_requested(&self) {
        self.metrics.lock().unwrap().keyframes += 1;
    }
    pub fn phone_rtt(&self, rtt_us: u64) {
        let mut m = self.metrics.lock().unwrap();
        m.phone_rtt_us = Some(rtt_us);
        m.phone_rtt_at = Some(Instant::now());
    }
    pub fn viewer_report(&self, r: ViewerReport) {
        let mut m = self.metrics.lock().unwrap();
        m.viewer_owner = None;
        m.viewer = Some((Instant::now(), r));
    }
    pub fn viewer_report_for(&self, client: &str, generation: u64, r: ViewerReport) {
        let mut m = self.metrics.lock().unwrap();
        m.viewer_owner = Some((client.to_owned(), generation));
        m.viewer = Some((Instant::now(), r));
    }
    /// Only the current authenticated viewer channel may drive automatic display changes.
    pub fn viewer_sample_for(&self, client: &str, generation: u64) -> Option<(Duration, ViewerReport)> {
        let m = self.metrics.lock().unwrap();
        if !m.viewer_owner.as_ref().is_some_and(|(id, g)| id == client && *g == generation) { return None; }
        m.viewer.as_ref().map(|(at, r)| (at.elapsed(), r.clone()))
    }
    pub fn viewer_caps(&self, caps: serde_json::Value) {
        self.metrics.lock().unwrap().caps = Some(caps);
    }
    pub fn caps(&self) -> Option<serde_json::Value> {
        self.metrics.lock().unwrap().caps.clone()
    }
    pub fn set_system(&self, system: serde_json::Value) {
        let mut m = self.metrics.lock().unwrap();
        m.system = system;
        m.system_at = Some(Instant::now());
    }
    pub fn system(&self) -> serde_json::Value {
        self.metrics.lock().unwrap().system.clone()
    }
    /// Called once per second by the service sampler.
    pub fn tick(&self, cpu: Option<f64>, temp: Option<f64>) {
        let mut m = self.metrics.lock().unwrap();
        let now = Instant::now();
        let (pf, pb, pt) = m.prev;
        if let Some(pt) = pt {
            let dt = now.duration_since(pt).as_secs_f64().max(0.001);
            m.fps_in = (m.frames_in - pf) as f64 / dt;
            m.bitrate_bps = (m.bytes_in - pb) as f64 * 8.0 / dt;
        }
        m.prev = (m.frames_in, m.bytes_in, Some(now));
        let fresh = m
            .viewer
            .as_ref()
            .filter(|(t, _)| t.elapsed() < Duration::from_secs(5))
            .map(|(_, r)| r.clone());
        let streaming = m
            .last_frame
            .is_some_and(|t| t.elapsed() < Duration::from_secs(5));
        let samples: [(&'static str, Option<f64>); 9] = [
            ("fps_in", streaming.then_some(m.fps_in)),
            ("bitrate_mbps", streaming.then_some(m.bitrate_bps / 1e6)),
            ("viewer_fps", fresh.as_ref().and_then(|r| r.fps)),
            ("decode_ms", fresh.as_ref().and_then(|r| r.decode_ms)),
            ("viewer_rtt_ms", fresh.as_ref().and_then(|r| r.rtt_ms)),
            (
                "phone_rtt_ms",
                m.phone_rtt_us.filter(|_| m.phone_rtt_at.is_some_and(|t| t.elapsed() < Duration::from_secs(5))).map(|u| u as f64 / 1000.0).filter(|_| streaming),
            ),
            ("dropped", Some(m.dropped as f64)),
            ("cpu", cpu),
            ("temp", temp),
        ];
        for (k, v) in samples {
            let h = m.history.entry(k).or_default();
            h.push_back(v);
            while h.len() > HISTORY {
                h.pop_front();
            }
        }
    }
    pub fn metrics(&self) -> serde_json::Value {
        let m = self.metrics.lock().unwrap();
        let viewer = m
            .viewer
            .as_ref()
            .map(|(t, r)| serde_json::json!({"age_ms": t.elapsed().as_millis() as u64, "report": r}));
        let since_frame = m.last_frame.map(|t| t.elapsed().as_millis() as u64);
        serde_json::json!({
            "server": {
                "frames_in": m.frames_in,
                "bytes_in": m.bytes_in,
                "dropped": m.dropped,
                "keyframe_requests": m.keyframes,
                "fps_in": m.fps_in,
                "bitrate_bps": m.bitrate_bps,
                "since_last_frame_ms": since_frame,
                "phone_rtt_ms": m.phone_rtt_us.filter(|_| m.phone_rtt_at.is_some_and(|t| t.elapsed() < Duration::from_secs(5))).map(|u| u as f64 / 1000.0),
            },
            "viewer": viewer,
            "system": m.system,
            "system_age_ms": m.system_at.map(|t| t.elapsed().as_millis() as u64),
            "history": m.history.iter().map(|(k, v)| (k.to_string(), v.iter().cloned().collect::<Vec<_>>())).collect::<BTreeMap<_,_>>(),
        })
    }
}

/// Human-readable timeline text for protocol status codes reported through `Hub::set_status`.
pub fn status_event(code: &str) -> Option<(&'static str, &'static str)> {
    let c = |p: &str| code.starts_with(p);
    Some(if c("WAITING_IPHONE_CONTROL") {
        ("info", "等待 iPhone 建立控制连接")
    } else if c("IPHONE_CONTROL_CONNECTED") {
        ("ok", "iPhone 控制连接已建立，开始鉴权")
    } else if c("CONTROL_VERIFIED") {
        ("ok", "控制通道配对验证通过")
    } else if c("IPHONE_RECORD_RECEIVED") {
        ("ok", "CarPlay 会话已建立，等待视频")
    } else if c("AUTHENTICATED_VIDEO_RECEIVED") {
        ("ok", "收到 iPhone 视频流")
    } else if c("REQUESTED_STREAMS_STOPPED") {
        ("warn", "iPhone 暂停了媒体流")
    } else if c("IPHONE_DISCONNECTED") {
        ("info", "iPhone 已断开，可重新连接")
    } else if c("IPHONE_PROTOCOL_OR_TRANSPORT_FAILED") {
        ("warn", "iPhone 协议或传输出错")
    } else if c("NATIVE_SESSION_TASK_FAILED") {
        ("error", "会话任务异常结束")
    } else if c("CONTROL_BACKPRESSURE") {
        ("warn", "控制通道积压，需要重连")
    } else {
        return None;
    })
}

/// Hide the middle octets of MAC addresses (aa:bb:cc:••:••:ff) inside free text.
pub fn mask_macs(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let hex = |c: u8| c.is_ascii_hexdigit();
    while i < b.len() {
        if i + 17 <= b.len()
            && (0..6).all(|k| hex(b[i + k * 3]) && hex(b[i + k * 3 + 1]))
            && (0..5).all(|k| matches!(b[i + k * 3 + 2], b':' | b'_' | b'-'))
            && (i == 0 || !hex(b[i - 1]))
            && (i + 17 == b.len() || !hex(b[i + 17]))
        {
            let sep = b[i + 2] as char;
            out.push_str(&text[i..i + 9]);
            out.push_str(&format!("••{sep}••{sep}"));
            out.push_str(&text[i + 15..i + 17]);
            i += 17;
        } else {
            let ch = text[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reconfiguration_during_owned_rotation_does_not_disable_logging() {
        use std::os::unix::fs::MetadataExt;
        let d=Diag::new();
        let dir=std::env::temp_dir().join(format!("cowplay-rotate-race-{}-{}",std::process::id(),now_ms()));
        std::fs::create_dir(&dir).unwrap();
        let path=dir.join("cowplay.log");
        d.set_sink(Some(path.clone())).unwrap();
        let file=d.sink.lock().unwrap().as_ref().unwrap().file.clone();
        let held=file.log.lock().unwrap();
        // Deterministically model archive's create-before-publish window while
        // its disk lock is held; configuration must not wait for that lock.
        file.rotating.store(true,Ordering::Release);
        std::fs::rename(&path,dir.join("cowplay.log.day-1-0")).unwrap();
        let replacement=std::fs::OpenOptions::new().create_new(true).append(true).open(&path).unwrap();
        let new_inode=replacement.metadata().unwrap().ino();
        d.set_sink_file(path,replacement,7).unwrap();
        assert_eq!(d.storage_status()["active"],true);
        assert_eq!(d.storage_status()["failures"],0);
        file.inode.store(new_inode,Ordering::Release);
        file.rotating.store(false,Ordering::Release);
        drop(held);
        assert!(d.shutdown_sink(tokio::time::Instant::now()+Duration::from_secs(2)).await);
        drop(file);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn replacing_active_log_inode_is_reported_instead_of_writing_an_unlinked_file() {
        let d=Diag::new();
        let dir=std::env::temp_dir().join(format!("cowplay-inode-{}-{}",std::process::id(),now_ms()));
        std::fs::create_dir(&dir).unwrap();
        let path=dir.join("cowplay.log");
        d.set_sink(Some(path.clone())).unwrap();
        std::fs::rename(&path,dir.join("external-backup")).unwrap();
        let replacement=std::fs::OpenOptions::new().create_new(true).append(true).open(&path).unwrap();
        assert!(d.set_sink_file(path,replacement,7).is_err());
        assert_eq!(d.storage_status()["active"],false);
        assert!(d.storage_status()["error"].as_str().unwrap().contains("替换"));
        let _=d.shutdown_sink(tokio::time::Instant::now()+Duration::from_secs(2)).await;
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn unavailable_destination_reports_first_open_and_replacement_failures() {
        let d=Diag::new();
        let dir=std::env::temp_dir().join(format!("cowplay-open-fail-{}-{}",std::process::id(),now_ms()));
        std::fs::create_dir(&dir).unwrap();
        assert!(d.set_sink(Some(dir.join("missing/cowplay.log"))).is_err());
        assert_eq!(d.storage_status()["failures"],1);
        assert_eq!(d.storage_status()["active"],false);
        d.set_sink(Some(dir.join("cowplay.log"))).unwrap();
        record_for(&d,Level::Info,"test","old destination line".into(),false);
        assert!(d.set_sink(Some(dir.join("missing/cowplay.log"))).is_err());
        assert!(d.sink_path().is_none(),"failed new config must not silently retain old destination");
        assert!(d.storage_status()["failures"].as_u64().unwrap()>0);
        let _=d.shutdown_sink(tokio::time::Instant::now()+Duration::from_secs(2)).await;
        assert!(std::fs::read_to_string(dir.join("cowplay.log")).unwrap().contains("old destination line"));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn disconnected_writer_and_mask_expansion_are_bounded_and_visible() {
        let d = Diag::new();
        let state = Arc::new(SinkState::default());
        let path = std::env::temp_dir().join(format!("cowplay-disconnected-{}",std::process::id()));
        let opened=std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap();
        let file=Arc::new(SinkFile{log:Mutex::new(crate::log_store::LogFile::new(path.clone(),opened,0).unwrap()),keep_days:AtomicU8::new(0),state:state.clone(),device:0,inode:Arc::new(AtomicU64::new(0)),rotating:Arc::new(AtomicBool::new(false))});
        let (tx,rx)=std::sync::mpsc::sync_channel(1); drop(rx);
        *d.sink.lock().unwrap()=Some(Sink{tx,path:path.clone(),file});
        *d.sink_state.lock().unwrap()=Some(state);
        record_for(&d,Level::Warn,"test","aa:bb:cc:dd:ee:ff ".repeat(100),false);
        let status=d.storage_status();
        assert_eq!(status["queue_dropped"],1);
        assert_eq!(status["failures"],1);
        assert_eq!(status["active"],false);
        let logs=d.logs(&LogQuery::default());
        assert!(logs.entries[0].msg.len()<=MSG_LIMIT+3);
        assert!(!logs.entries[0].msg.contains("aa:bb:cc:dd:ee:ff"));
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn write_error_is_reported_without_erasing_existing_disk_logs() {
        let path=std::env::temp_dir().join(format!("cowplay-readonly-{}",std::process::id()));
        std::fs::write(&path, "existing\n").unwrap();
        let opened=std::fs::File::open(&path).unwrap();
        let state=Arc::new(SinkState::default());
        let file=Arc::new(SinkFile{log:Mutex::new(crate::log_store::LogFile::new(path.clone(),opened,0).unwrap()),keep_days:AtomicU8::new(0),state:state.clone(),device:0,inode:Arc::new(AtomicU64::new(0)),rotating:Arc::new(AtomicBool::new(false))});
        let (tx,rx)=std::sync::mpsc::sync_channel(1);
        tx.send("failed write\n".to_owned()).unwrap(); drop(tx);
        sink_thread(file,rx);
        assert_eq!(state.failures.load(Ordering::Relaxed),1);
        assert!(state.error.lock().unwrap().is_some());
        assert_eq!(state.last_sync_ms.load(Ordering::Relaxed),0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(),"existing\n");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn memory_ring_reports_eviction_and_retained_sequence() {
        let d=Diag::new();
        for _ in 0..4200 {record_for(&d,Level::Info,"test","x".repeat(MSG_LIMIT),false);}
        let logs=d.logs(&LogQuery::default());
        assert!(logs.evicted>0);
        assert_eq!(logs.oldest_seq,Some(logs.evicted+1));
        assert_eq!(logs.last_seq,4200);
        assert!(logs.bytes<=LOG_CAP_BYTES);
    }
    #[tokio::test]
    async fn persistent_sink_drains_and_syncs_final_line() {
        let d = Diag::new();
        let dir = std::env::temp_dir().join(format!("cowplay-drain-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("cowplay.log");
        d.set_sink(Some(path.clone())).unwrap();
        for n in 0..128 { record_for(&d, Level::Info, "test", format!("final-marker {n}"), false); }
        assert!(d.shutdown_sink(tokio::time::Instant::now()+Duration::from_secs(2)).await);
        let state = d.storage_status();
        assert_eq!(state["queued"], 128);
        assert_eq!(state["written"], 128);
        assert_eq!(state["queue_dropped"], 0);
        assert!(state["last_sync_ms"].as_u64().unwrap() > 0, "sync completion is acknowledged, not inferred from read-back");
        assert!(std::fs::read_to_string(path).unwrap().contains("final-marker 127"));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn full_queue_and_expired_drain_budget_remain_visible() {
        let d = Diag::new();
        let dir = std::env::temp_dir().join(format!("cowplay-full-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir(&dir).unwrap();
        d.set_sink(Some(dir.join("cowplay.log"))).unwrap();
        let file = d.sink.lock().unwrap().as_ref().unwrap().file.clone();
        let held = file.log.lock().unwrap();
        for n in 0..4200 { record_for(&d, Level::Info, "test", format!("queued {n}"), false); }
        assert!(d.storage_status()["queue_dropped"].as_u64().unwrap() > 0);
        assert!(!d.shutdown_sink(tokio::time::Instant::now()).await);
        assert!(d.storage_status()["error"].as_str().unwrap().contains("停服预算"));
        drop(held);
        assert!(!d.shutdown_sink(tokio::time::Instant::now()+Duration::from_secs(2)).await, "the earlier timeout remains a reported failure");
        let state = d.storage_status();
        assert_eq!(state["written"], state["queued"]);
        assert!(state["last_sync_ms"].as_u64().unwrap()>0);
        drop(file);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn idle_writer_syncs_dirty_data_without_a_second_log_line() {
        let d = Diag::new();
        let dir = std::env::temp_dir().join(format!("cowplay-sync-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir(&dir).unwrap();
        d.set_sink(Some(dir.join("cowplay.log"))).unwrap();
        record_for(&d, Level::Info, "test", "single dirty line".into(), false);
        tokio::time::timeout(Duration::from_secs(2), async {
            while d.storage_status()["last_sync_ms"].as_u64().unwrap()==0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("writer must acknowledge its timed native sync without another line");
        assert!(d.shutdown_sink(tokio::time::Instant::now()+Duration::from_secs(2)).await);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn mac_masking_keeps_vendor_and_last_octet() {
        assert_eq!(
            mask_macs("rfcomm a4:83:E7:12:34:21 ok"),
            "rfcomm a4:83:E7:••:••:21 ok"
        );
        assert_eq!(mask_macs("/dev_A4_83_E7_12_34_21"), "/dev_A4_83_E7_••_••_21");
        assert_eq!(mask_macs("no mac 12:34 here"), "no mac 12:34 here");
        assert_eq!(mask_macs("中文 aa:bb:cc:dd:ee:ff。"), "中文 aa:bb:cc:••:••:ff。");
    }
    #[test]
    fn ring_filters_levels_modules_and_search() {
        let _serial = SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
        let d = diag();
        d.set_verbose(true);
        record(Level::Debug, "test-a", "debug line alpha".into(), false);
        record(Level::Error, "test-a", "error line beta".into(), false);
        record(Level::Info, "test-b", "info line Gamma".into(), false);
        d.set_verbose(false);
        record(Level::Debug, "test-a", "dropped while quiet".into(), false);
        let page = d.logs(&LogQuery {
            module: Some("test-a".into()),
            ..Default::default()
        });
        assert_eq!(page.matched, 2);
        assert_eq!(page.entries[0].msg, "error line beta");
        let page = d.logs(&LogQuery {
            level: Some("warn".into()),
            module: Some("test-a".into()),
            ..Default::default()
        });
        assert_eq!(page.matched, 1);
        let page = d.logs(&LogQuery {
            q: Some("gamma".into()),
            ..Default::default()
        });
        assert_eq!(page.matched, 1);
        assert!(page.bytes <= LOG_CAP_BYTES);
        record(Level::Info,"masked-log-test","peer /org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF aa:bb:cc:dd:ee:ff".into(),false);
        let page=d.logs(&LogQuery{module:Some("masked-log-test".into()),..Default::default()});
        assert_eq!(page.entries[0].msg,"peer /org/bluez/hci0/dev_AA_BB_CC_••_••_FF aa:bb:cc:••:••:ff");
    }
    static SESSIONS: Mutex<()> = Mutex::new(());
    #[test]
    fn automatic_display_samples_require_the_exact_viewer_channel() {
        let _serial = SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
        let d = diag();
        let old = { let mut m = d.metrics.lock().unwrap(); (m.viewer.take(), m.viewer_owner.take()) };
        let report = || ViewerReport {fps: Some(60.0), jitter_ms: Some(20.0), visible: Some(true), ..Default::default()};
        d.viewer_report_for("synthetic-screen", 4, report());
        assert!(d.viewer_sample_for("synthetic-screen", 4).is_some());
        assert!(d.viewer_sample_for("synthetic-screen", 5).is_none(), "a new channel for the same screen must not reuse old metrics");
        assert!(d.viewer_sample_for("other-screen", 4).is_none());
        d.viewer_report(report());
        assert!(d.viewer_sample_for("synthetic-screen", 4).is_none(), "unowned diagnostic samples cannot reconnect a phone");
        let mut m = d.metrics.lock().unwrap(); m.viewer = old.0; m.viewer_owner = old.1;
    }
    #[test]
    fn session_lifecycle_and_history() {
        let _serial = SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
        let d = diag();
        let id = d.session_begin("test");
        assert_eq!(d.session_begin("again"), id);
        d.event("warn", "video", "slow");
        d.first_frame();
        d.first_frame();
        d.stall(2400);
        d.session_end("test end");
        let (current, history) = d.sessions();
        assert!(current.is_none());
        let s = history.iter().find(|s| s.id == id).unwrap();
        assert_eq!(s.stalls, 1);
        assert_eq!(s.warnings, 2);
        assert!(s.first_frame_ms.is_some());
        assert_eq!(
            s.events.iter().filter(|e| e.msg.contains("首帧")).count(),
            1
        );
    }
    #[test]
    fn bluetooth_only_session_ends_but_control_session_survives() {
        let _serial = SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
        let d = diag();
        d.session_end("reset");
        let first = d.session_begin("rfcomm");
        d.session_end_without_control("bt only");
        assert!(d.session_id().is_none(), "a Bluetooth-only attempt must not stay open");
        assert!(d.sessions().1.iter().any(|s| s.id == first));
        d.session_begin("rfcomm");
        d.set_peer("192.0.2.1".into(), None);
        d.session_end_without_control("bt closed after handoff");
        assert!(d.session_id().is_some(), "a session with a control connection continues");
        d.session_end("test");
    }
    #[test]
    fn persistent_sink_masks_identifiers_before_writing() {
        let _serial = SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("cowplay-mask-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cowplay.log");
        diag().set_sink(Some(file.clone())).unwrap();
        record(Level::Warn, "test-s", "synthetic peer aa:bb:cc:dd:ee:ff".into(), false);
        diag().set_sink(None).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        let text = loop {
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            if text.contains("synthetic peer") || Instant::now() >= until { break text; }
            std::thread::yield_now();
        };
        let _ = std::fs::remove_dir_all(dir);
        assert!(text.contains("aa:bb:cc:••:••:ff"), "persistent storage must contain the masked form");
        assert!(!text.contains("aa:bb:cc:dd:ee:ff"));
    }
    #[test]
    fn file_sink_receives_lines_off_thread() {
        let _serial = SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("cowplay-sink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cowplay.log");
        diag().set_sink(Some(file.clone())).unwrap();
        record(Level::Warn, "test-s", "sink line marker".into(), false);
        diag().set_sink(None).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until && !std::fs::read_to_string(&file).unwrap_or_default().contains("sink line marker") {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(std::fs::read_to_string(&file).unwrap().contains("sink line marker"));
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn reopening_a_log_with_a_draining_queue_preserves_both_queues_across_rotation() {
        let _serial = SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("cowplay-sink-reopen-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("cowplay.log");
        std::fs::write(&path, "yesterday\n").unwrap();
        let day = now_ms() / 86_400_000;
        std::fs::OpenOptions::new().write(true).open(&path).unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs((day - 1) * 86_400))).unwrap();
        diag().set_sink_with_retention(Some(path.clone()), 1).unwrap();
        let (old, file) = { let sink = diag().sink.lock().unwrap(); let s = sink.as_ref().unwrap(); (s.tx.clone(), s.file.clone()) };
        let held = file.log.lock().unwrap();
        old.send("pending old queue one\n".into()).unwrap();
        diag().set_sink(None).unwrap();
        diag().set_sink_with_retention(Some(dir.join("./cowplay.log")), 1).unwrap();
        let new = diag().sink.lock().unwrap().as_ref().unwrap().tx.clone();
        new.send("new queue\n".into()).unwrap();
        old.send("pending old queue two\n".into()).unwrap();
        drop(held); drop(old); drop(new);
        diag().set_sink(None).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        let complete = || { let text = std::fs::read_to_string(&path).unwrap_or_default(); ["pending old queue one", "pending old queue two", "new queue"].iter().all(|line| text.contains(line)) };
        while Instant::now() < until && (!complete() || Arc::strong_count(&file) > 1) { std::thread::sleep(Duration::from_millis(10)); }
        assert!(complete(), "today's old and new queued lines must survive reopening: {:?}", std::fs::read_to_string(&path));
        assert_eq!(Arc::strong_count(&file), 1, "both detached queues have finished before cleanup");
        assert!(!std::fs::read_to_string(&path).unwrap().contains("yesterday"));
        drop(file);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn disabled_debug_does_not_evaluate_arguments_or_write_any_sink() {
        let _serial = SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
        diag().set_verbose(false);
        let evaluated = std::cell::Cell::new(false);
        crate::diag_echo!(Debug, "test-lazy", "{}", { evaluated.set(true); "should be skipped" });
        assert!(!evaluated.get());
        assert_eq!(diag().logs(&LogQuery { module: Some("test-lazy".into()), ..Default::default() }).matched, 0);
    }
    #[test]
    fn long_message_is_truncated_on_char_boundary() {
        let _serial = SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
        diag().set_verbose(false);
        record(Level::Info, "test-c", "汉".repeat(600), false);
        let page = diag().logs(&LogQuery {
            module: Some("test-c".into()),
            ..Default::default()
        });
        assert!(page.entries[0].msg.len() <= MSG_LIMIT + 3);
        let logs = diag().logs.lock().unwrap();
        let stored = logs.entries.iter().find(|entry| entry.module == "test-c").unwrap();
        assert!(stored.msg.capacity() <= MSG_LIMIT + 3, "retained allocation must match the bounded message, capacity={}", stored.msg.capacity());
    }
}
