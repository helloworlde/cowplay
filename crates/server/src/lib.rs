//! Reusable HTTPS/WSS bridge. Does not claim an authenticated CarPlay session.
pub mod auth;
pub mod build_info;
pub mod clients;
pub mod clock;
pub mod diag;
mod log_store;
pub use log_store::archive_key as log_archive_key;
pub mod ultra;
pub mod vehicle;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use f50_core::media::{touch_report, video_wire_owned, Contact, Frame, VideoQueue};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{mpsc, Notify};
static WEB: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../web/dist");
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub bind: SocketAddr,
    pub origin: String,
    pub tls_certificate: String,
    pub tls_private_key: String,
    pub access_token: String,
}
impl Config {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.origin.starts_with("https://")
            || self.origin.ends_with('/')
            || !f50_core::validation::access_token(&self.access_token)
        {
            return Err("invalid origin or token length");
        }
        if self.tls_certificate == self.tls_private_key {
            return Err("certificate and key paths must differ");
        }
        Ok(())
    }
}
struct AudioQueue {
    configs: std::collections::BTreeMap<u8, Vec<u8>>,
    packets: std::collections::VecDeque<Vec<u8>>,
    bytes: usize,
    generation: u64,
}
impl AudioQueue {
    /// Drop queued packets and make the viewer reload every stream configuration.
    fn restart(&mut self) {
        self.packets.clear();
        self.bytes = 0;
        self.generation = self.generation.wrapping_add(1);
    }
}
struct AudioFeedback {
    rate: u32,
    id: u64,
    first: Option<u32>,
    latest_us: u64,
    played: Option<(u64, std::time::Instant)>,
}
pub const WS_WRITE_LIMIT: usize = f50_core::media::MAX_MEDIA + 65536 + 64;
pub struct Hub {
    pub vehicle: Arc<vehicle::Feed>,
    session_epoch: AtomicU64,
    last_keyframe: Mutex<Option<std::time::Instant>>,
    audio: Mutex<AudioQueue>,
    pub phone_active: AtomicBool,
    video_received: AtomicBool,
    display_report: Mutex<&'static str>,
    pub clock: Mutex<clock::Clock>,
    audio_feedback: Mutex<std::collections::BTreeMap<u8, AudioFeedback>>,
    queue: Mutex<VideoQueue>,
    config: Mutex<Option<Vec<u8>>>,
    notify: Notify,
    viewer: AtomicBool,
    pending_connections: Arc<tokio::sync::Semaphore>,
    pub clients: Arc<clients::Clients>,
    origin: String,
    pub control: mpsc::Sender<Control>,
    pub simulated: AtomicBool,
    pub phones: Arc<phones::Phones>,
    pub phone_verified: AtomicBool,
    session_status: Mutex<&'static str>,
    dimensions: Mutex<(u16, u16)>,
    video_epoch: AtomicU64,
    viewer_generation: AtomicU64,
    host_ui_epoch: AtomicU64,
    night: AtomicBool,
    pair_setup_len: Mutex<Duration>,
    planned_restart: AtomicBool,
    pair_setup_until: Mutex<Option<std::time::Instant>>,
    display_spec: Mutex<Option<DisplaySpec>>,
    codec: std::sync::atomic::AtomicU8,
    mic: Mutex<Option<MicUplink>>,
    mic_epoch: AtomicU64,
    /// Cookie id of the screen currently showing CarPlay.
    viewer_client: Mutex<Option<String>>,
    /// Bumped to ask the viewing car page to reload itself (phone remote).
    reload_epoch: AtomicU64,
}
/// Phone microphone input stream (telephony / speech) negotiated in an audio SETUP.
pub struct MicTarget {
    pub kind: u8,
    pub audio_type: String,
    pub addr: SocketAddr,
    pub key: [u8; 32],
    pub rate: u32,
    pub channels: u8,
}
struct MicUplink {
    target: MicTarget,
    socket: Option<std::net::UdpSocket>,
    counters: f50_core::audio::MicCounters,
    pending: Vec<u8>,
    packets: u64,
}
#[derive(Debug)]
pub enum Control {
    Touch([u8; 12]),
    /// One complete Home press/release, serialized together on the event channel.
    Home,
    Knob([u8; 4]),
    Media(u8),
    Siri,
    Keyframe,
    ViewerDisconnected,
    Iap(Vec<u8>),
    NightMode(bool),
    /// Abort the active phone session so the next connection re-reads /info.
    EndSession,
}
/// Display declaration offered to the phone (PlayPort display settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DisplaySpec {
    pub width: u16,
    pub height: u16,
    pub fps: u8,
    pub width_mm: u16,
    pub hevc: bool,
    /// Calls/Siri via the car browser microphone (declares telephony/speech input).
    pub microphone: bool,
    pub right_hand_drive: bool,
}
impl Hub {
    pub fn observation(&self) -> serde_json::Value {
        serde_json::json!({"verified":self.phone_verified.load(Ordering::Acquire),"phone":self.phone_active.load(Ordering::Acquire),"viewer":self.viewer.load(Ordering::Acquire),"video":self.video_received.load(Ordering::Acquire),"display":*self.display_report.lock().unwrap(),"protocol":*self.session_status.lock().unwrap()})
    }
    fn display(&self, state: &str) {
        let code = match state {
            "rendered" if self.video_received.load(Ordering::Acquire) => "BROWSER_FRAME_RENDERED",
            "error" => "BROWSER_DISPLAY_ERROR",
            _ => return,
        };
        let mut report = self.display_report.lock().unwrap();
        if *report != code {
            if code == "BROWSER_FRAME_RENDERED" {
                diag::diag().first_frame();
            } else {
                diag::diag().event("warn", "web", "车机浏览器报告画面无法呈现");
            }
        }
        *report = code;
    }
    pub fn audio_metadata(&self, kind: u8, rate: u32, id: u64) {
        self.audio_feedback.lock().unwrap().insert(
            kind,
            AudioFeedback {
                rate,
                id,
                first: None,
                latest_us: 0,
                played: None,
            },
        );
    }
    pub fn audio_received(&self, kind: u8, sample: u32, timestamp_us: u64) {
        if let Some(m) = self.audio_feedback.lock().unwrap().get_mut(&kind) {
            m.first.get_or_insert(sample);
            m.latest_us = timestamp_us;
        }
    }
    fn audio_progress(&self, kind: u8, timestamp_us: u64) {
        if let Some(m) = self.audio_feedback.lock().unwrap().get_mut(&kind) {
            if timestamp_us <= m.latest_us.saturating_add(1_000_000)
                && m.played
                    .as_ref()
                    .is_none_or(|(last, _)| timestamp_us >= *last)
            {
                m.played = Some((timestamp_us, std::time::Instant::now()));
            }
        }
    }
    pub fn feedback(&self) -> Vec<f50_core::bplist::Value> {
        use f50_core::bplist::{dict, number};
        let clock = self.clock.lock().unwrap();
        self.audio_feedback
            .lock()
            .unwrap()
            .iter()
            .map(|(kind, m)| {
                let mut entries = vec![
                    ("type", number(*kind as u64)),
                    ("sampleRate", number(m.rate as u64)),
                    ("streamConnectionID", number(m.id)),
                ];
                if self.viewer.load(Ordering::Acquire) && clock.synced() {
                    if let (Some(first), Some((played, when))) = (m.first, m.played) {
                        if when.elapsed() < Duration::from_secs(2) {
                            let samples = ((played as u128 * m.rate as u128) / 1_000_000) as u32;
                            entries
                                .push(("sampleTime", number(first.wrapping_add(samples) as u64)));
                            // Map browser completion arrival into the synchronized phone domain. Network latency remains unmeasured.
                            let age = ((when.elapsed().as_nanos() << 32) / 1_000_000_000) as u64;
                            entries.push(("timestamp", number(clock.now().wrapping_sub(age))));
                        }
                    }
                }
                dict(entries)
            })
            .collect()
    }
    pub fn audio_config(&self, kind: u8, wire: Vec<u8>) -> Result<(), &'static str> {
        if !(100..=102).contains(&kind) || wire.len() > 256 {
            return Err("invalid audio config");
        }
        let mut a = self.audio.lock().unwrap();
        a.configs.insert(kind, wire);
        a.restart();
        self.notify.notify_one();
        Ok(())
    }
    pub fn audio_packet(&self, wire: Vec<u8>) {
        if !self.viewer.load(Ordering::Acquire) || wire.len() > 4096 {
            return;
        }
        let mut a = self.audio.lock().unwrap();
        if a.packets.len() >= 64 || a.bytes + wire.len() > 262144 {
            a.restart();
        }
        a.bytes += wire.len();
        a.packets.push_back(wire);
        self.notify.notify_one();
    }

    pub fn set_dimensions(&self, width: u16, height: u16) -> Result<(), &'static str> {
        if width == 0 || height == 0 || width > f50_core::info::MAX_WIDTH || height > f50_core::info::MAX_HEIGHT {
            return Err("invalid display size");
        }
        *self.dimensions.lock().unwrap() = (width, height);
        Ok(())
    }
    pub fn set_status(&self, status: &'static str) {
        let mut s = self.session_status.lock().unwrap();
        if *s != status {
            *s = status;
            self.notify.notify_one();
            drop(s);
            if let Some((kind, text)) = diag::status_event(status) {
                diag::diag().event(kind, "carplay", text);
            }
        }
    }
    pub fn new(token: String, origin: String) -> (Arc<Self>, mpsc::Receiver<Control>) {
        Self::with_phones(token, origin, phones::Phones::new(None, None))
    }
    pub fn with_phones(token: String, origin: String, phones: Arc<phones::Phones>) -> (Arc<Self>, mpsc::Receiver<Control>) {
        let (tx, rx) = mpsc::channel(32);
        (
            Arc::new(Self {
                vehicle: vehicle::Feed::new(),
                session_epoch: AtomicU64::new(0),
                last_keyframe: Mutex::new(None),
                phone_active: AtomicBool::new(false),
                video_received: AtomicBool::new(false),
                display_report: Mutex::new("WAITING_BROWSER_REPORT"),
                clock: Mutex::new(clock::Clock::default()),
                audio_feedback: Mutex::new(Default::default()),
                audio: Mutex::new(AudioQueue {
                    configs: Default::default(),
                    packets: Default::default(),
                    bytes: 0,
                    generation: 0,
                }),
                queue: Mutex::new(VideoQueue::new(8 * 1024 * 1024, 32)),
                config: Mutex::new(None),
                notify: Notify::new(),
                viewer: AtomicBool::new(false),
                pending_connections: Arc::new(tokio::sync::Semaphore::new(8)),
                clients: Arc::new(clients::Clients::new(&token)),
                origin,
                control: tx,
                simulated: AtomicBool::new(false),
                phones,
                phone_verified: AtomicBool::new(false),
                session_status: Mutex::new("DIAGNOSTIC_BRIDGE; LIVE_SESSION_NOT_CONNECTED"),
                dimensions: Mutex::new((800, 480)),
                video_epoch: AtomicU64::new(0),
                viewer_generation: AtomicU64::new(0),
                host_ui_epoch: AtomicU64::new(0),
                night: AtomicBool::new(false),
                pair_setup_len: Mutex::new(Duration::ZERO),
                planned_restart: AtomicBool::new(false),
                pair_setup_until: Mutex::new(None),
                display_spec: Mutex::new(None),
                codec: std::sync::atomic::AtomicU8::new(1),
                mic: Mutex::new(None),
                mic_epoch: AtomicU64::new(0),
                viewer_client: Mutex::new(None),
                reload_epoch: AtomicU64::new(0),
            }),
            rx,
        )
    }
    pub fn set_config(&self, config: Vec<u8>) -> Result<(), &'static str> {
        if config.len() > 65536 {
            return Err("configuration too large");
        }
        if let Some(&codec) = config.get(2) {
            self.codec.store(codec, Ordering::Release);
        }
        *self.config.lock().unwrap() = Some(config);
        self.queue.lock().unwrap().discontinuity();
        self.notify.notify_one();
        Ok(())
    }
    pub fn publish_reusable(&self, data: &mut Vec<u8>, pts_us: u64) -> bool {
        self.video_received.store(true, Ordering::Release);
        let size = data.len();
        if !self.viewer.load(Ordering::Acquire) {
            diag::diag().frame_in(size, 0);
            return false;
        }
        let key = if self.codec.load(Ordering::Acquire) == 2 {
            f50_core::media::is_keyframe_hevc(data)
        } else {
            f50_core::media::is_keyframe(data)
        };
        let (accepted, waiting, dropped) = {
            let mut q = self.queue.lock().unwrap();
            let before = q.dropped;
            let ok = q.push_reusable(data, pts_us, key);
            (ok, q.needs_keyframe(), q.dropped - before)
        };
        diag::diag().frame_in(size, dropped);
        if waiting {
            let mut last = self.last_keyframe.lock().unwrap();
            if last.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
                *last = Some(std::time::Instant::now());
                diag::diag().keyframe_requested();
                let _ = self.control.try_send(Control::Keyframe);
            }
        }
        self.notify.notify_one();
        accepted
    }
    pub fn publish(&self, frame: Frame) -> bool {
        self.video_received.store(true, Ordering::Release);
        let size = frame.data.len();
        if !self.viewer.load(Ordering::Acquire) {
            diag::diag().frame_in(size, 0);
            return false;
        }
        let (accepted, dropped) = {
            let mut q = self.queue.lock().unwrap();
            let before = q.dropped;
            (q.push(frame), q.dropped - before)
        };
        diag::diag().frame_in(size, dropped);
        self.notify.notify_one();
        accepted
    }
    pub fn stop_audio(&self, kind: u8) {
        self.mic_stop(Some(kind));
        self.audio_feedback.lock().unwrap().remove(&kind);
        let mut a = self.audio.lock().unwrap();
        a.configs.remove(&kind);
        a.restart();
        self.notify.notify_one();
    }
    /// The CowPlay icon on the CarPlay home screen was tapped (`requestUI`): open car settings.
    pub fn request_host_ui(&self) {
        self.host_ui_epoch.fetch_add(1, Ordering::AcqRel);
    }
    /// Drop queued video, make the viewer reset its decoder and request a fresh IDR frame.
    /// The phone session and the viewer connection stay up.
    pub fn restart_video(&self) -> bool {
        if !self.viewer.load(Ordering::Acquire) {
            return false;
        }
        // Bump the epoch first so no keyframe queued after the flush can precede `video-restart`.
        self.video_epoch.fetch_add(1, Ordering::AcqRel);
        self.queue.lock().unwrap().discontinuity();
        diag::diag().keyframe_requested();
        let _ = self.control.try_send(Control::Keyframe);
        self.notify.notify_one();
        diag::diag().event("info", "video", "已重启车机视频管线（重置解码器并请求关键帧）");
        true
    }
    pub fn display_spec(&self) -> Option<DisplaySpec> {
        *self.display_spec.lock().unwrap()
    }
    /// Store a new declaration; returns true when it differs from the previous one.
    pub fn set_display_spec(&self, spec: DisplaySpec) -> bool {
        self.display_spec.lock().unwrap().replace(spec) != Some(spec)
    }
    /// End the live session on purpose (new display/voice declaration). The receiver reports it as
    /// a planned restart rather than a failure.
    pub fn end_session(&self) -> bool {
        if !self.phone_active.load(Ordering::Acquire) {
            return false;
        }
        self.planned_restart.store(true, Ordering::Release);
        let sent = self.control.try_send(Control::EndSession).is_ok();
        if !sent {
            self.planned_restart.store(false, Ordering::Release);
        }
        sent
    }
    pub fn has_phone_session(&self) -> bool { self.phone_active.load(Ordering::Acquire) }
    pub fn pause_phone(&self) -> std::io::Result<u64> {
        let epoch = self.phones.pause()?;
        *self.pair_setup_until.lock().unwrap() = None;
        Ok(epoch)
    }
    pub fn pause_phone_if_current(&self,expected:u64)->std::io::Result<u64> {
        let epoch=self.phones.pause_if_current(expected)?;
        *self.pair_setup_until.lock().unwrap()=None;
        Ok(epoch)
    }
    pub fn take_planned_restart(&self) -> bool {
        self.planned_restart.swap(false, Ordering::AcqRel)
    }
    /// Start sending browser microphone audio to the phone's input stream.
    pub fn mic_start(&self, target: MicTarget) {
        let socket = std::net::UdpSocket::bind(if target.addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })
            .and_then(|s| s.set_nonblocking(true).map(|_| s))
            .ok();
        diag::diag().event("info", "audio", format!("iPhone 打开{}麦克风输入（{} Hz）", if target.audio_type == "telephony" { "通话" } else { "Siri" }, target.rate));
        *self.mic.lock().unwrap() = Some(MicUplink { target, socket, counters: Default::default(), pending: Vec::new(), packets: 0 });
        self.mic_epoch.fetch_add(1, Ordering::AcqRel);
        self.notify.notify_one();
    }
    pub fn mic_stop(&self, kind: Option<u8>) {
        let mut mic = self.mic.lock().unwrap();
        if mic.as_ref().is_some_and(|m| kind.is_none_or(|k| m.target.kind == k)) {
            let sent = mic.take().map_or(0, |m| m.packets);
            drop(mic);
            diag::diag().event("info", "audio", format!("麦克风输入结束（已发送 {sent} 包）"));
            self.mic_epoch.fetch_add(1, Ordering::AcqRel);
            self.notify.notify_one();
        }
    }
    fn mic_state(&self) -> serde_json::Value {
        match self.mic.lock().unwrap().as_ref() {
            Some(m) => serde_json::json!({"type":"mic","state":"start","rate":m.target.rate,"channels":m.target.channels,"audioType":m.target.audio_type}),
            None => serde_json::json!({"type":"mic","state":"stop"}),
        }
    }
    /// Browser PCM16LE mono at the negotiated rate → 20 ms big-endian RTP packets to the phone.
    fn mic_pcm(&self, pcm_le: &[u8]) {
        let mut guard = self.mic.lock().unwrap();
        let Some(m) = guard.as_mut() else { return };
        let frame = (m.target.rate as usize / 50) * 2 * m.target.channels as usize;
        if m.pending.len() > frame * 25 {
            m.pending.clear(); // never let a stalled uplink build latency
        }
        m.pending.extend(pcm_le.chunks_exact(2).flat_map(|s| [s[1], s[0]]));
        while m.pending.len() >= frame {
            let body: Vec<u8> = m.pending.drain(..frame).collect();
            let samples = (frame / 2 / m.target.channels as usize) as u32;
            if let (Ok(packet), Some(socket)) = (
                f50_core::audio::seal_mic_packet(&m.target.key, m.target.kind, &mut m.counters, &body, samples),
                m.socket.as_ref(),
            ) {
                if socket.send_to(&packet, m.target.addr).is_ok() {
                    m.packets += 1;
                }
            }
        }
    }
    pub fn night_mode(&self) -> bool {
        self.night.load(Ordering::Acquire)
    }
    /// Length of each CarPlay pair-setup window (config `pairing_window_seconds`, at most 120 s).
    pub fn set_pair_setup_length(&self, length: Duration) {
        *self.pair_setup_len.lock().unwrap() = length.min(Duration::from_secs(120));
    }
    /// Re-open CarPlay pair-setup. Called at service start and whenever a bonded phone has just
    /// passed iAP2 MFi authentication over Bluetooth, so a later reconnect is not refused.
    pub fn open_pair_setup(&self) {
        let length = *self.pair_setup_len.lock().unwrap();
        if length.is_zero() {
            return;
        }
        let until = std::time::Instant::now() + length;
        let mut slot = self.pair_setup_until.lock().unwrap();
        if slot.is_none_or(|current| current < until) {
            *slot = Some(until);
        }
    }
    pub fn pair_setup_remaining(&self) -> Duration {
        self.pair_setup_until
            .lock()
            .unwrap()
            .map_or(Duration::ZERO, |until| until.saturating_duration_since(std::time::Instant::now()))
    }
    /// Store the head-unit appearance and push it to a live session (PlayPort `setNightMode`).
    pub fn set_night_mode(&self, night: bool) {
        if self.night.swap(night, Ordering::AcqRel) != night && self.phone_active.load(Ordering::Acquire) {
            let _ = self.control.try_send(Control::NightMode(night));
            diag::diag().event("info", "carplay", if night { "已请求 CarPlay 夜间外观" } else { "已请求 CarPlay 日间外观" });
        }
    }
    pub fn viewer_active(&self) -> bool {
        self.viewer.load(Ordering::Acquire)
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    /// Cookie id of the screen currently showing CarPlay.
    pub fn viewer_client(&self) -> Option<String> {
        self.viewer.load(Ordering::Acquire).then(|| self.viewer_client.lock().unwrap().clone()).flatten()
    }
    fn claim_viewer(&self, id: &str) -> Option<u64> {
        let mut viewer = self.viewer_client.lock().unwrap();
        self.viewer.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).ok()?;
        let generation = self.viewer_generation.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        *viewer = Some(id.to_owned()); Some(generation)
    }
    pub fn viewer_generation(&self) -> u64 { self.viewer_generation.load(Ordering::Acquire) }
    pub fn media_generation(&self) -> u64 { self.session_epoch.load(Ordering::Acquire) }
    /// Serialize an automatic display commit with phone selection, viewer replacement
    /// and media resets. The callback must not acquire Phones or viewer_client again.
    pub fn with_display_context<R>(&self, phone_epoch: u64, screen: &str, viewer: u64, media: u64, commit: impl FnOnce() -> R) -> Option<R> {
        self.phones.while_current(phone_epoch, || {
            let id = self.viewer_client.lock().unwrap();
            (self.viewer.load(Ordering::Acquire) && id.as_deref() == Some(screen)
                && self.viewer_generation() == viewer && self.media_generation() == media
                && self.has_phone_session() && self.phone_verified.load(Ordering::Acquire)).then(commit)
        }).flatten()
    }
    /// Ask the viewing car page to reload (phone remote "刷新车机页面").
    pub fn request_reload(&self) -> bool {
        self.reload_epoch.fetch_add(1, Ordering::AcqRel);
        self.notify.notify_waiters();
        self.viewer.load(Ordering::Acquire)
    }
    /// Drop the current viewer (car "断开" from another device); its page shows the stopped state.
    pub fn release_viewer(&self) -> bool {
        let _viewer = self.viewer_client.lock().unwrap();
        if !self.viewer.load(Ordering::Acquire) {
            return false;
        }
        self.viewer_generation.fetch_add(1, Ordering::AcqRel);
        self.notify.notify_waiters();
        true
    }
    pub fn video_active(&self) -> bool {
        self.video_received.load(Ordering::Acquire)
    }
    pub fn status_code(&self) -> &'static str {
        *self.session_status.lock().unwrap()
    }
    pub fn dimensions(&self) -> (u16, u16) {
        *self.dimensions.lock().unwrap()
    }
    pub fn queue_bytes(&self) -> usize {
        self.queue.lock().unwrap().bytes()
    }
    pub fn clear_video(&self) {
        self.video_received.store(false, Ordering::Release);
        *self.display_report.lock().unwrap() = "WAITING_BROWSER_REPORT";
        self.queue.lock().unwrap().discontinuity();
        *self.config.lock().unwrap() = None;
        self.notify.notify_one();
    }
    pub fn reset(&self) {
        let viewer = self.viewer_client.lock().unwrap();
        self.phone_verified.store(false, Ordering::Release);
        self.session_epoch.fetch_add(1, Ordering::AcqRel);
        drop(viewer);
        self.mic_stop(None);
        self.video_received.store(false, Ordering::Release);
        *self.display_report.lock().unwrap() = "WAITING_BROWSER_REPORT";
        self.audio_feedback.lock().unwrap().clear();
        {
            let mut a = self.audio.lock().unwrap();
            a.configs.clear();
            a.restart();
        }
        self.queue.lock().unwrap().discontinuity();
        *self.config.lock().unwrap() = None;
        self.notify.notify_one();
    }
}
pub fn router(hub: Arc<Hub>) -> Router {
    Router::new()
        .route("/ws", get(ws_upgrade))
        .merge(auth::router())
        .route(
            "/health",
            get(|| async { "bridge available; query authenticated websocket for session status" }),
        )
        .fallback(get(asset))
        .with_state(hub)
}
async fn asset(State(hub): State<Arc<Hub>>, uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if matches!(path, "" | "index.html" | "pairing" | "pairing.html") {
        let target = if path.starts_with("pairing") { "/pair" } else { "/car" };
        return axum::response::Redirect::temporary(target).into_response();
    }
    if matches!(path, "admin" | "admin.html") {
        let host = hub.origin().parse::<axum::http::Uri>().ok().and_then(|u| u.host().map(str::to_owned));
        return match host {
            Some(host) => axum::response::Redirect::temporary(&format!("http://{host}/cgi-bin/luci/admin/services/cowplay")).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        };
    }
    let page = format!("{path}.html");
    let name = if path == "pair" {
        "phone.html"
    } else if !path.contains('.') && WEB.get_file(&page).is_some() {
        page.as_str()
    } else {
        path
    };
    match WEB.get_file(name) {
        Some(f) => {
            let mime = if name.ends_with(".js") {
                "text/javascript"
            } else if name.ends_with(".css") {
                "text/css"
            } else if name.ends_with(".svg") {
                "image/svg+xml"
            } else {
                "text/html; charset=utf-8"
            };
            ([(axum::http::header::CONTENT_TYPE,mime),(axum::http::header::CACHE_CONTROL,"no-store"),(axum::http::header::REFERRER_POLICY,"no-referrer"),(axum::http::header::CONTENT_SECURITY_POLICY,"default-src 'self'; connect-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self'; object-src 'none'; frame-ancestors 'none'")],f.contents()).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn ws_upgrade(
    State(h): State<Arc<Hub>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if headers.get("origin").and_then(|v| v.to_str().ok()) != Some(&h.origin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(viewer) = h.clients.from_headers(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let permit = match h.pending_connections.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return StatusCode::TOO_MANY_REQUESTS.into_response(),
    };
    ws.write_buffer_size(65536)
        .max_write_buffer_size(WS_WRITE_LIMIT)
        .max_message_size(4096)
        .max_frame_size(4096)
        .on_upgrade(move |s| async move {
            let _permit = permit;
            client(s, h, viewer.id).await
        })
}
struct ViewerGuard(Arc<Hub>);
impl Drop for ViewerGuard {
    fn drop(&mut self) {
        let viewer = self.0.viewer_client.lock().unwrap();
        self.0.viewer.store(false, Ordering::Release);
        drop(viewer);
        *self.0.display_report.lock().unwrap() = "WAITING_BROWSER_REPORT";
        {
            let mut a = self.0.audio.lock().unwrap();
            a.packets.clear();
            a.bytes = 0;
        }
        self.0.queue.lock().unwrap().discontinuity();
        let _ = self.0.control.try_send(Control::ViewerDisconnected);
        diag::diag().event("info", "web", "车机查看通道已断开");
    }
}
async fn send(s: &mut WebSocket, m: Message) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(3), s.send(m)).await,
        Ok(Ok(()))
    )
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    build: String,
    protocol: u32,
    /// The user chose this screen (tapped connect): replace the current viewer instead of waiting.
    #[serde(default)]
    takeover: bool,
    /// Browser window and negotiated picture region of this screen.
    #[serde(default)]
    window: Option<clients::Window>,
}
#[cfg(test)]
fn test_cookie(h: &Hub) -> String {
    let c = h.clients.admit_token(&h.clients.token(), "screen", "test", "", "").unwrap();
    format!("{}={}", clients::COOKIE, c.id)
}
#[cfg(test)]
fn handshake() -> String {
    serde_json::json!({"build":build_info::BUILD_ID,"protocol":build_info::PROTOCOL})
        .to_string()
}
async fn client(mut socket: WebSocket, h: Arc<Hub>, id: String) {
    let auth = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await;
    let hello = match auth {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<Hello>(&t).ok(),
        _ => None,
    };
    let Some(hello) = hello.filter(|h| h.protocol == build_info::PROTOCOL && h.build == build_info::BUILD_ID) else {
        let mismatch = serde_json::json!({"type":"error","code":"CLIENT_BUILD_MISMATCH","message":"页面版本不匹配，请刷新网页。","build":build_info::BUILD_ID,"protocol":build_info::PROTOCOL});
        let _ = send(&mut socket, Message::Text(mismatch.to_string().into())).await;
        return;
    };
    if let Some(window) = hello.window {
        h.clients.set_window(&id, window);
    }
    let mut generation = h.claim_viewer(&id);
    if generation.is_none() {
        // Automatic retries wait; only an explicit takeover replaces an active screen.
        if hello.takeover {
            {
                let _viewer = h.viewer_client.lock().unwrap();
                h.viewer_generation.fetch_add(1, Ordering::AcqRel);
            }
            h.notify.notify_waiters();
            for _ in 0..30 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                generation = h.claim_viewer(&id);
                if generation.is_some() { break; }
            }
        }
        if generation.is_none() {
            let _=send(&mut socket,Message::Text(r#"{"type":"error","code":"VIEWER_BUSY","message":"另一块屏幕正在显示 CarPlay。"}"#.into())).await;
            return;
        }
        diag::diag().event("info", "web", "新的车机屏幕接管了显示");
    }
    let generation = generation.unwrap();
    let _guard = ViewerGuard(h.clone());
    diag::diag().event("ok", "web", "车机查看通道已连接");
    if !send(&mut socket,Message::Text(serde_json::json!({"type":"hello","build":build_info::BUILD_ID,"protocol":build_info::PROTOCOL}).to_string().into())).await{return}
    let mut epoch = h.session_epoch.load(Ordering::Acquire);
    h.queue.lock().unwrap().discontinuity();
    let _ = h.control.try_send(Control::Keyframe);
    let mut previous: Option<Vec<u8>> = None;
    let mut previous_status = "";
    let mut audio_generation = Some(0);
    let mut video_epoch = h.video_epoch.load(Ordering::Acquire);
    let mut host_ui_epoch = h.host_ui_epoch.load(Ordering::Acquire);
    let mut reload_epoch = h.reload_epoch.load(Ordering::Acquire);
    let mut authorization_check = tokio::time::interval(Duration::from_secs(1));
    let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(30), Duration::from_secs(30));
    // Only announce microphone state when an input stream exists or changes.
    let mut mic_epoch = h.mic_epoch.load(Ordering::Acquire);
    if h.mic.lock().unwrap().is_some() && !send(&mut socket, Message::Text(h.mic_state().to_string().into())).await {
        return;
    }
    loop {
        if h.clients.get(&id).is_none() {
            let _ = send(&mut socket, Message::Text(r#"{"type":"error","code":"AUTH_REVOKED","message":"这块屏幕的授权已失效，请重新输入访问令牌。"}"#.into())).await;
            return;
        }
        let current_mic = h.mic_epoch.load(Ordering::Acquire);
        if current_mic != mic_epoch {
            mic_epoch = current_mic;
            if !send(&mut socket, Message::Text(h.mic_state().to_string().into())).await {
                return;
            }
        }
        if h.viewer_generation.load(Ordering::Acquire) != generation {
            let _ = send(&mut socket, Message::Text(r#"{"type":"error","code":"VIEWER_REPLACED","message":"已在另一块屏幕显示。"}"#.into())).await;
            return;
        }
        if h.reload_epoch.load(Ordering::Acquire) != reload_epoch {
            reload_epoch = h.reload_epoch.load(Ordering::Acquire);
            if !send(&mut socket, Message::Text(r#"{"type":"reload"}"#.into())).await {
                return;
            }
        }
        let current_ui = h.host_ui_epoch.load(Ordering::Acquire);
        if current_ui != host_ui_epoch {
            host_ui_epoch = current_ui;
            if !send(&mut socket, Message::Text(r#"{"type":"host-ui"}"#.into())).await {
                return;
            }
        }
        let current_video = h.video_epoch.load(Ordering::Acquire);
        if current_video != video_epoch {
            video_epoch = current_video;
            previous = None;
            if !send(&mut socket, Message::Text(r#"{"type":"video-restart"}"#.into())).await {
                return;
            }
        }
        let current_epoch = h.session_epoch.load(Ordering::Acquire);
        if current_epoch != epoch {
            epoch = current_epoch;
            previous = None;
            audio_generation = None;
            if !send(
                &mut socket,
                Message::Text(r#"{"type":"session-reset"}"#.into()),
            )
            .await
            {
                return;
            }
        }
        let status = if h.simulated.load(Ordering::Relaxed) {
            "SYNTHETIC MEDIA TEST — no iPhone session"
        } else {
            *h.session_status.lock().unwrap()
        };
        if status != previous_status {
            if !send(
                &mut socket,
                Message::Text(
                    serde_json::json!({"type":"status","message":status})
                        .to_string()
                        .into(),
                ),
            )
            .await
            {
                return;
            }
            previous_status = status;
        }
        let config = h.config.lock().unwrap().clone();
        if config != previous {
            if let Some(ref b) = config {
                if !send(&mut socket, Message::Binary(b.clone().into())).await {
                    return;
                }
            }
            if config.is_none() && previous.is_some() {
                if !send(
                    &mut socket,
                    Message::Text(r#"{"type":"video-stop"}"#.into()),
                )
                .await
                {
                    return;
                }
            }
            previous = config;
        }
        let (current_audio_generation, configs, packet) = {
            let mut a = h.audio.lock().unwrap();
            let configs = if audio_generation != Some(a.generation) {
                Some(a.configs.values().cloned().collect::<Vec<_>>())
            } else {
                None
            };
            let packet = a.packets.pop_front();
            if let Some(p) = &packet {
                a.bytes -= p.len();
            }
            (a.generation, configs, packet)
        };
        if let Some(configs) = configs {
            for kind in 100..=102 {
                if !send(&mut socket, Message::Binary(vec![6, kind].into())).await {
                    return;
                }
            }
            for config in configs {
                if !send(&mut socket, Message::Binary(config.into())).await {
                    return;
                }
            }
            audio_generation = Some(current_audio_generation);
        }
        let had_audio = packet.is_some();
        if let Some(packet) = packet {
            if !send(&mut socket, Message::Binary(packet.into())).await {
                return;
            }
        }
        let frame = h.queue.lock().unwrap().pop();
        let had_frame = frame.is_some();
        if let Some(f) = frame {
            if !send(
                &mut socket,
                Message::Binary(video_wire_owned(f.data, f.pts_us, f.key).into()),
            )
            .await
            {
                return;
            }
        }
        tokio::select! {
         msg=socket.recv()=>{if !viewer_message(&mut socket,&h,&id,generation,msg,&mut previous).await{return}}
         _=h.notify.notified()=>{},
         _=std::future::ready(()), if had_frame||had_audio=>{},
         _=authorization_check.tick()=>{}
         _=heartbeat.tick()=>{if !send(&mut socket,Message::Ping(vec![].into())).await{return}}
        }
    }
}
/// Apply one message from the viewing page; `false` closes the viewer connection.
async fn viewer_message(
    socket: &mut WebSocket,
    h: &Hub,
    id: &str,
    generation: u64,
    msg: Option<Result<Message, axum::Error>>,
    previous: &mut Option<Vec<u8>>,
) -> bool {
    // Ownership may change while the outer loop awaits this socket.
    if h.viewer_generation()!=generation || h.viewer_client().as_deref()!=Some(id) || h.clients.get(id).is_none() { return false; }
    match msg {
        Some(Ok(Message::Text(t))) => {
            if t.len() > 4096 {
                return false;
            }
            let Ok(input) = serde_json::from_str::<Input>(&t) else {
                return false;
            };
            match input {
                Input::Home => {
                    if (h.phone_active.load(Ordering::Acquire) || h.simulated.load(Ordering::Relaxed))
                        && h.control.try_send(Control::Home).is_err() { return false; }
                }
                Input::Knob { knob } => {
                    let Ok(report) = f50_core::media::knob_report(&knob) else { return false; };
                    if (h.phone_active.load(Ordering::Acquire) || h.simulated.load(Ordering::Relaxed))
                        && h.control.try_send(Control::Knob(report)).is_err() { return false; }
                }
                Input::Media { media } => {
                    if !(3..=5).contains(&media) { return false; }
                    if (h.phone_active.load(Ordering::Acquire) || h.simulated.load(Ordering::Relaxed))
                        && h.control.try_send(Control::Media(media)).is_err() { return false; }
                }
                Input::Siri => {
                    if (h.phone_active.load(Ordering::Acquire) || h.simulated.load(Ordering::Relaxed))
                        && h.control.try_send(Control::Siri).is_err() { return false; }
                }
                Input::Touch { contacts } => {
                    let (width, height) = *h.dimensions.lock().unwrap();
                    let Ok(report) = touch_report(&contacts, width, height) else {
                        return false;
                    };
                    if h.control.try_send(Control::Touch(report)).is_err() {
                        return false;
                    }
                }
                Input::Keyframe => {
                    h.queue.lock().unwrap().discontinuity();
                    // A closed browser decoder needs the codec configuration again.
                    *previous = None;
                    diag::diag().keyframe_requested();
                    if h.control.try_send(Control::Keyframe).is_err() {
                        return false;
                    }
                }
                Input::Metrics { report } => {
                    if h.viewer_generation() == generation && h.viewer_client().as_deref() == Some(id) {
                        diag::diag().viewer_report_for(id, generation, report);
                    }
                }
                Input::Caps { caps } => {
                    if caps.to_string().len() <= 3000 {
                        diag::diag().viewer_caps(caps)
                    }
                }
                Input::Window { window } => h.clients.set_window(id, window),
                Input::Stall { ms } => diag::diag().stall(ms.min(600_000)),
                Input::Display { state } => h.display(&state),
                Input::Audioprogress { kind, timestamp_us } => h.audio_progress(kind, timestamp_us),
                Input::Ping => return send(socket, Message::Text(r#"{"type":"pong"}"#.into())).await,
            }
            true
        }
        // Browser microphone PCM (tag 8); any other binary input is a protocol error.
        Some(Ok(Message::Binary(b))) => {
            if b.first() != Some(&8) {
                return false;
            }
            h.mic_pcm(&b[1..]);
            true
        }
        Some(Ok(Message::Ping(b))) => send(socket, Message::Pong(b)).await,
        Some(Ok(Message::Pong(_))) => true,
        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => false,
    }
}
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Input {
    Knob { knob: f50_core::media::KnobState },
    Media { media: u8 },
    Siri,
    Touch { contacts: Vec<Contact> },
    Home,
    Keyframe,
    Display { state: String },
    Audioprogress { kind: u8, timestamp_us: u64 },
    Ping,
    Metrics { report: diag::ViewerReport },
    Caps { caps: serde_json::Value },
    Stall { ms: u64 },
    Window { window: clients::Window },
}
pub async fn run(
    config: Config,
    hub: Arc<Hub>,
    handle: Handle,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_with_router(config, hub, handle, Router::new()).await
}
pub async fn run_with_router(
    config: Config,
    hub: Arc<Hub>,
    handle: Handle,
    extra: Router,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    config.validate()?;
    // No generated certificate, trust-store mutation, or insecure fallback.
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        &config.tls_certificate,
        &config.tls_private_key,
    )
    .await?;
    let _ = TLS.set(tls.clone());
    axum_server::bind_rustls(config.bind, tls)
        .handle(handle)
        .serve(router(hub).merge(extra).into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}
pub type Handle = axum_server::Handle<SocketAddr>;
static TLS: std::sync::OnceLock<axum_server::tls_rustls::RustlsConfig> = std::sync::OnceLock::new();
/// Swap the serving certificate in place (renewed certificate); existing connections continue.
pub async fn reload_tls(certificate: &str, private_key: &str) -> std::io::Result<()> {
    match TLS.get() {
        Some(tls) => tls.reload_from_pem_file(certificate, private_key).await,
        None => Err(std::io::Error::other("TLS server not started")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    #[test]
    fn browser_report_cannot_precede_video_and_disconnect_clears_it() {
        let (h, _) = Hub::new(
            "synthetic-token-for-tests-only".into(),
            "https://test.invalid".into(),
        );
        h.display("rendered");
        assert_eq!(h.observation()["display"], "WAITING_BROWSER_REPORT");
        h.video_received.store(true, Ordering::Relaxed);
        h.display("rendered");
        assert_eq!(h.observation()["display"], "BROWSER_FRAME_RENDERED");
        h.display("error");
        assert_eq!(h.observation()["display"], "BROWSER_DISPLAY_ERROR");
        drop(ViewerGuard(h.clone()));
        assert_eq!(h.observation()["display"], "WAITING_BROWSER_REPORT");
    }
    #[test]
    fn feedback_requires_synced_clock_and_recent_browser_completion() {
        let (h, _) = Hub::new(
            "synthetic-token-for-tests-only".into(),
            "https://test.invalid".into(),
        );
        h.audio_metadata(100, 48000, 7);
        h.audio_received(100, 9000, 1_000_000);
        h.viewer.store(true, Ordering::Relaxed);
        h.audio_progress(100, 500_000);
        assert!(h.feedback()[0]
            .as_dictionary()
            .unwrap()
            .get("sampleTime")
            .is_none());
        {
            let mut c = h.clock.lock().unwrap();
            let t = c.request();
            assert!(c.response(t, t, t, t));
        }
        assert_eq!(
            h.feedback()[0].as_dictionary().unwrap()["sampleTime"].as_unsigned_integer(),
            Some(33000)
        );
        h.audio_progress(100, 99_000_000);
        assert_eq!(
            h.feedback()[0].as_dictionary().unwrap()["sampleTime"].as_unsigned_integer(),
            Some(33000)
        );
        h.viewer.store(false, Ordering::Relaxed);
        assert!(h.feedback()[0]
            .as_dictionary()
            .unwrap()
            .get("sampleTime")
            .is_none());
        h.stop_audio(100);
        assert!(h.feedback().is_empty());
    }
    #[test]
    fn bluetooth_handoff_reopens_carplay_pair_setup() {
        let (h, _) = Hub::new("synthetic-token-for-tests-only".into(), "https://example.invalid".into());
        h.open_pair_setup();
        assert_eq!(h.pair_setup_remaining(), Duration::ZERO, "disabled until a length is configured");
        h.set_pair_setup_length(Duration::from_secs(600));
        h.open_pair_setup();
        let first = h.pair_setup_remaining();
        assert!(first > Duration::from_secs(119) && first <= Duration::from_secs(120), "capped at 120 s");
        *h.pair_setup_until.lock().unwrap() = Some(std::time::Instant::now());
        assert_eq!(h.pair_setup_remaining(), Duration::ZERO, "expired window refuses pair-setup");
        h.open_pair_setup();
        assert!(h.pair_setup_remaining() > Duration::from_secs(119), "a later iAP2 handoff re-opens it");
    }
    #[test]
        fn browser_mic_pcm_becomes_sealed_big_endian_rtp() {
        let (h, _) = Hub::new("synthetic-token-for-tests-only".into(), "https://example.invalid".into());
        let phone = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        phone.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        h.mic_start(MicTarget { kind: 100, audio_type: "telephony".into(), addr: phone.local_addr().unwrap(), key: [9; 32], rate: 16000, channels: 1 });
        let pcm: Vec<u8> = (0..320u16).flat_map(|i| i.to_le_bytes()).collect();
        h.mic_pcm(&pcm[..300]);
        h.mic_pcm(&pcm[300..]);
        let mut buf = [0u8; 2048];
        let n = phone.recv(&mut buf).unwrap();
        let packet = &buf[..n];
        assert_eq!(&packet[..2], &[0x80, 100]);
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&packet[n - 8..]);
        let plain = f50_core::crypto::open(&[9; 32], &nonce, &packet[12..n - 8], &packet[4..12]).unwrap();
        assert_eq!(plain.len(), 640, "20 ms of 16 kHz mono");
        assert_eq!(&plain[2..4], &1u16.to_be_bytes(), "samples are big-endian on the wire");
        h.mic_stop(Some(100));
        h.mic_pcm(&pcm);
        assert!(phone.recv(&mut buf).is_err(), "nothing is sent after the phone closes the input stream");
    }
#[tokio::test]
    async fn static_and_health() {
        let (h, _) = Hub::new(
            "synthetic-token-for-tests-only".into(),
            "https://example.invalid".into(),
        );
        let r = router(h);
        let result = r
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.status(), StatusCode::OK);
        for (path, title) in [("/car", "CowPlay · 车机"), ("/phone", "CowPlay 手机"), ("/pair", "CowPlay 手机")] {
            let page = router(Hub::new("synthetic-token-for-tests-only".into(), "https://example.invalid".into()).0)
                .oneshot(axum::http::Request::builder().uri(path).body(axum::body::Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(page.status(), StatusCode::OK, "{path}");
            assert!(page.headers()[axum::http::header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
            let body = axum::body::to_bytes(page.into_body(), 1 << 20).await.unwrap();
            assert!(String::from_utf8_lossy(&body).contains(title), "{path}");
        }
        for (path, target) in [("/", "/car"), ("/pairing", "/pair"), ("/admin", "http://example.invalid/cgi-bin/luci/admin/services/cowplay")] {
            let page = router(h_missing()).oneshot(axum::http::Request::builder().uri(path).body(axum::body::Body::empty()).unwrap()).await.unwrap();
            assert_eq!(page.status(), StatusCode::TEMPORARY_REDIRECT);
            assert_eq!(page.headers()[axum::http::header::LOCATION], target);
        }
        let missing = router(h_missing())
            .oneshot(axum::http::Request::builder().uri("/nope").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }
    fn h_missing() -> Arc<Hub> {
        Hub::new("synthetic-token-for-tests-only".into(), "https://example.invalid".into()).0
    }
    #[test]
    fn congestion_counts_evicted_frames_even_when_current_idr_is_accepted() {
        // Metrics are process-global. Isolate this regression from concurrent media tests
        // so an unrelated queue overflow cannot make a broken publisher appear to pass.
        if std::env::var_os("COWPLAY_METRICS_TEST_ISOLATED").is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::congestion_counts_evicted_frames_even_when_current_idr_is_accepted"])
                .env("COWPLAY_METRICS_TEST_ISOLATED", "1").output().unwrap();
            assert!(result.status.success(), "{}{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
            return;
        }
        let (hub, _) = Hub::new("synthetic-token-for-tests-only".into(), "https://example.invalid".into());
        hub.viewer.store(true, Ordering::Release);
        for i in 0..32 {
            assert!(hub.publish(Frame { data: vec![0, 0, 0, 1, 0x65, i], pts_us: 0, key: true }));
        }
        let before = diag::diag().metrics()["server"]["dropped"].as_u64().unwrap();
        let mut idr = vec![0, 0, 0, 1, 0x65, 1];
        assert!(hub.publish_reusable(&mut idr, 0));
        assert_eq!(diag::diag().metrics()["server"]["dropped"].as_u64().unwrap(), before + 32);
        let mut queue = hub.queue.lock().unwrap();
        assert!(queue.pop().unwrap().key);
        assert!(queue.pop().is_none());
    }
    #[test]
    fn strict_configuration() {
        let c = Config {
            bind: "127.0.0.1:8443".parse().unwrap(),
            origin: "http://invalid".into(),
            tls_certificate: "cert".into(),
            tls_private_key: "key".into(),
            access_token: "a".repeat(24),
        };
        assert!(c.validate().is_err());
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{client::IntoClientRequest, Message as WsMessage},
    };
    #[tokio::test]
    async fn verified_tls_slow_stopped_and_recovery() {
        use tokio::io::AsyncReadExt;
        use tokio_rustls::{TlsConnector, rustls::pki_types::{CertificateDer, ServerName}};
        let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec!["test.invalid".into()]).unwrap();
        let (h, _rx) = Hub::new("synthetic-token-for-tests-only".into(), "https://test.invalid".into());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(cert.pem().into_bytes(), signing_key.serialize_pem().into_bytes()).await.unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(cert.der().to_vec())).unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions().unwrap().with_root_certificates(roots).with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(config));
        let app = router(h.clone());
        let task = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls).unwrap().serve(app.into_make_service()).await.unwrap();
        });
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        assert!(connector.connect(ServerName::try_from("wrong.invalid").unwrap(), tcp).await.is_err(), "hostname verification must stay enabled");
        let connect = || async {
            let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
            let tls = connector.connect(ServerName::try_from("test.invalid").unwrap(), tcp).await.unwrap();
            let mut request = format!("wss://test.invalid:{}/ws", addr.port()).into_client_request().unwrap();
            request.headers_mut().insert("Origin", "https://test.invalid".parse().unwrap());
            request.headers_mut().insert("Cookie", test_cookie(&h).parse().unwrap());
            let (mut ws, _) = tokio_tungstenite::client_async(request, tls).await.unwrap();
            ws.send(WsMessage::Text(handshake().into())).await.unwrap();
            assert!(ws.next().await.unwrap().unwrap().to_text().unwrap().contains("hello"));
            ws
        };
        for slow in [true, false] {
            let mut ws = connect().await;
            let reader = tokio::spawn(async move {
                if slow {
                    let mut buf = [0u8; 1024];
                    loop {
                        if ws.get_mut().read(&mut buf).await.unwrap_or(0) == 0 { break; }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                } else {
                    std::future::pending::<()>().await;
                    drop(ws);
                }
            });
            let started = std::time::Instant::now();
            let mut peak = 0;
            while h.viewer.load(Ordering::Acquire) && started.elapsed() < Duration::from_secs(6) {
                h.publish(Frame { data: vec![7; f50_core::media::MAX_MEDIA], pts_us: 0, key: true });
                peak = peak.max(h.queue.lock().unwrap().bytes());
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            assert!(!h.viewer.load(Ordering::Acquire), "TLS slow/stopped reader must release its slot");
            assert_eq!(h.queue.lock().unwrap().bytes(), 0);
            assert!(peak <= f50_core::media::MAX_MEDIA);
            reader.abort();
        }
        // A fresh trusted connection receives media after both timed-out readers.
        let mut ws = connect().await;
        h.publish(Frame { data: vec![0, 0, 0, 1, 0x65, 1], pts_us: 42, key: true });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop { if matches!(ws.next().await, Some(Ok(WsMessage::Binary(_)))) { break; } }
        }).await.unwrap();
        ws.close(None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while h.viewer.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.unwrap();
        task.abort();
    }
    #[tokio::test]
    async fn version_rejection_reset_and_stopped_reader_release() {
        let (h, _control) = Hub::new(
            "synthetic-token-for-tests-only".into(),
            "https://test.invalid".into(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(h.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let request = || {
            let mut r = format!("ws://{addr}/ws").into_client_request().unwrap();
            r.headers_mut()
                .insert("Origin", "https://test.invalid".parse().unwrap());
            r.headers_mut().insert("Cookie", test_cookie(&h).parse().unwrap());
            r
        };
        let (mut stale, _) = connect_async(request()).await.unwrap();
        stale
            .send(WsMessage::Text("not-a-hello".into()))
            .await
            .unwrap();
        assert!(stale
            .next()
            .await
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap()
            .contains("CLIENT_BUILD_MISMATCH"));
        assert!(!h.viewer.load(Ordering::Acquire));
        drop(stale);
        let (mut ws, _) = connect_async(request()).await.unwrap();
        ws.send(WsMessage::Text(
            handshake().into(),
        ))
        .await
        .unwrap();
        assert!(ws
            .next()
            .await
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap()
            .contains("hello"));
        let _ = ws.next().await;
        h.reset();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(Ok(WsMessage::Text(t))) = ws.next().await {
                    if t.contains("session-reset") {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        // Real loopback socket, synthetic bytes; deliberately stop reading while the server writes.
        let start = std::time::Instant::now();
        let mut maximum = 0;
        while h.viewer.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(9) {
            h.publish(Frame {
                data: vec![7; f50_core::media::MAX_MEDIA],
                pts_us: 0,
                key: true,
            });
            maximum = maximum.max(h.queue.lock().unwrap().bytes());
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        assert!(
            !h.viewer.load(Ordering::Acquire),
            "stopped reader must time out"
        );
        assert_eq!(h.queue.lock().unwrap().bytes(), 0);
        assert!(maximum <= f50_core::media::MAX_MEDIA);
        eprintln!(
            "stopped-reader: elapsed_ms={} max_queued_bytes={} final_queued_bytes=0 write_limit={}",
            start.elapsed().as_millis(),
            maximum,
            WS_WRITE_LIMIT
        );
        drop(ws);
        task.abort();
    }
    #[tokio::test]
    async fn automatic_display_commit_rejects_superseded_phone_viewer_and_media_tokens() {
        for change in ["phone", "viewer", "media"] {
            let (h, mut control) = Hub::new("synthetic-token-for-tests-only".into(), "https://test.invalid".into());
            let phone = "/org/bluez/hci0/dev_00_11_22_33_44_55";
            h.phones.initialize(Some(phone)).unwrap(); assert!(h.phones.handoff(h.phones.epoch(), phone));
            h.phone_active.store(true, Ordering::Release); h.phone_verified.store(true, Ordering::Release);
            let epoch = h.phones.epoch(); let viewer = h.claim_viewer("synthetic-screen").unwrap(); let media = h.media_generation();
            assert_eq!(h.with_display_context(epoch, "synthetic-screen", viewer, media, || true), Some(true));
            match change { "phone" => {h.pause_phone().unwrap();}, "viewer" => {h.release_viewer();}, _ => h.reset() }
            assert_eq!(h.with_display_context(epoch, "synthetic-screen", viewer, media, || h.end_session()), None, "stale {change} decision must not reach EndSession");
            assert!(control.try_recv().is_err()); assert!(!h.take_planned_restart());
        }
    }
    #[tokio::test]
    async fn ordinary_viewer_reconnect_does_not_reuse_old_channel_metrics() {
        let (h, _control) = Hub::new("synthetic-token-for-tests-only".into(), "https://test.invalid".into());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(h.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let cookie = test_cookie(&h);
        let request = || {
            let mut r = format!("ws://{addr}/ws").into_client_request().unwrap();
            r.headers_mut().insert("Origin", "https://test.invalid".parse().unwrap());
            r.headers_mut().insert("Cookie", cookie.parse().unwrap()); r
        };
        let hello = serde_json::json!({"build":build_info::BUILD_ID,"protocol":build_info::PROTOCOL}).to_string();
        let (mut old, _) = connect_async(request()).await.unwrap();
        old.send(WsMessage::Text(hello.clone().into())).await.unwrap();
        assert!(old.next().await.unwrap().unwrap().to_text().unwrap().contains("hello"));
        let first = h.viewer_generation(); let id = h.viewer_client().unwrap();
        let metrics = serde_json::json!({"type":"metrics","report":{"fps":60.0,"ping":{"seq":1,"rtt_ms":40.0,"age_ms":0},"visible":true}}).to_string();
        assert!(serde_json::from_str::<Input>(&metrics).is_ok(), "the real browser metrics shape must parse");
        old.send(WsMessage::Text(metrics.into())).await.unwrap();
        let received = tokio::time::timeout(Duration::from_secs(2), async {
            while diag::diag().viewer_sample_for(&id, first).is_none() { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await;
        assert!(received.is_ok(), "channel: generation={}/{first}, viewer={}, sample={}", h.viewer_generation(), h.viewer_client().as_deref()==Some(id.as_str()), diag::diag().metrics()["viewer"]);
        old.close(None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while h.viewer.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.unwrap();
        let (mut new, _) = connect_async(request()).await.unwrap();
        new.send(WsMessage::Text(hello.into())).await.unwrap();
        assert!(new.next().await.unwrap().unwrap().to_text().unwrap().contains("hello"));
        assert_eq!(h.viewer_client().as_deref(), Some(id.as_str()));
        assert_ne!(h.viewer_generation(), first, "a normal reconnect needs a distinct channel identity");
        assert!(diag::diag().viewer_sample_for(&id, h.viewer_generation()).is_none());
        let media = h.media_generation(); h.reset(); assert_ne!(h.media_generation(), media);
        new.close(None).await.unwrap(); task.abort();
    }
    #[tokio::test]
    async fn user_connect_takes_over_the_screen_and_tells_the_old_one() {
        let (h, _control) = Hub::new("synthetic-token-for-tests-only".into(), "https://test.invalid".into());
        h.simulated.store(true, Ordering::Relaxed);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(h.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let request = || {
            let mut request = format!("ws://{addr}/ws").into_client_request().unwrap();
            request.headers_mut().insert("Origin", "https://test.invalid".parse().unwrap());
            request.headers_mut().insert("Cookie", test_cookie(&h).parse().unwrap());
            request
        };
        let hello = |takeover: bool| serde_json::json!({"build":build_info::BUILD_ID,"protocol":build_info::PROTOCOL,"takeover":takeover}).to_string();
        let (mut old, _) = connect_async(request()).await.unwrap();
        old.send(WsMessage::Text(hello(false).into())).await.unwrap();
        assert!(old.next().await.unwrap().unwrap().to_text().unwrap().contains("hello"));
        // An automatic retry from another screen still waits.
        let (mut retry, _) = connect_async(request()).await.unwrap();
        retry.send(WsMessage::Text(hello(false).into())).await.unwrap();
        assert!(retry.next().await.unwrap().unwrap().to_text().unwrap().contains("VIEWER_BUSY"));
        // The user's own connect replaces the old screen.
        let (mut new, _) = connect_async(request()).await.unwrap();
        new.send(WsMessage::Text(hello(true).into())).await.unwrap();
        let mut replaced = false;
        while let Some(Ok(m)) = tokio::time::timeout(Duration::from_secs(3), old.next()).await.ok().flatten() {
            if m.to_text().map(|t| t.contains("VIEWER_REPLACED")).unwrap_or(false) { replaced = true; break; }
        }
        assert!(replaced, "the old screen is told why it stopped");
        let first = tokio::time::timeout(Duration::from_secs(4), new.next()).await.unwrap().unwrap().unwrap();
        assert!(first.to_text().unwrap().contains("hello"), "the new screen gets the session: {first:?}");
        let id = h.viewer_client.lock().unwrap().clone().unwrap();
        let handle = h.clients.get(&id).unwrap().handle();
        assert!(h.clients.remove(&handle).unwrap());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let message = new.next().await.unwrap().unwrap();
                if message.to_text().is_ok_and(|t| t.contains("AUTH_REVOKED")) { break; }
            }
        }).await.expect("revocation must terminate an otherwise idle viewer promptly");
        assert!(h.clients.get(&id).is_none());
        task.abort();
    }
    #[tokio::test]
    async fn loopback_synthetic_media_and_touch() {
        let (h, mut control) = Hub::new(
            "synthetic-token-for-tests-only".into(),
            "https://test.invalid".into(),
        );
        h.simulated.store(true, Ordering::Relaxed);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(h.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let request = || {
            let mut request = format!("ws://{addr}/ws").into_client_request().unwrap();
            request.headers_mut().insert("Origin", "https://test.invalid".parse().unwrap());
            request.headers_mut().insert("Cookie", test_cookie(&h).parse().unwrap());
            request
        };
        let (mut ws, _) = connect_async(request()).await.unwrap();
        ws.send(WsMessage::Text(
            handshake().into(),
        ))
        .await
        .unwrap();
        let hello = ws.next().await.unwrap().unwrap();
        assert!(hello.to_text().unwrap().contains(build_info::BUILD_ID));
        let status = ws.next().await.unwrap().unwrap();
        assert!(status.to_text().unwrap().contains("SYNTHETIC"));
        {
            let (mut blocked, _) = connect_async(request()).await.unwrap();
            blocked.send(WsMessage::Text(handshake().into())).await.unwrap();
            let error = blocked.next().await.unwrap().unwrap();
            let value: serde_json::Value = serde_json::from_str(error.to_text().unwrap()).unwrap();
            assert_eq!(value["code"], "VIEWER_BUSY");
            assert!(h.viewer.load(Ordering::Acquire), "rejected client must not release the current viewer");
            let mut anonymous = request();
            anonymous.headers_mut().remove("Cookie");
            assert!(connect_async(anonymous).await.is_err(), "no authorized client cookie: upgrade refused");
            assert!(h.viewer.load(Ordering::Acquire));
        }
        ws.send(WsMessage::Pong(vec![7].into())).await.unwrap();
        ws.send(WsMessage::Text(r#"{"type":"ping"}"#.into()))
            .await
            .unwrap();
        let pong = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(pong.to_text().unwrap().contains("pong"));
        assert!(matches!(control.recv().await, Some(Control::Keyframe)));
        let config = vec![
            1, 110, 1, 32, 3, 0, 0, 224, 1, 0, 0, 1, 66, 0, 30, 255, 225, 0, 1, 103,
        ];
        h.set_config(config.clone()).unwrap();
        assert_eq!(
            ws.next().await.unwrap().unwrap().into_data().as_ref(),
            config
        );
        assert!(h.publish(Frame {
            data: vec![0, 0, 0, 1, 101, 1],
            pts_us: 123456,
            key: true
        }));
        let f = ws.next().await.unwrap().unwrap().into_data();
        assert_eq!(u64::from_le_bytes(f[3..11].try_into().unwrap()), 123456);
        // A closed browser decoder needs its configuration again, even if its bytes
        // are unchanged. A keyframe alone cannot reconstruct that decoder.
        ws.send(WsMessage::Text(r#"{"type":"keyframe"}"#.into()))
            .await
            .unwrap();
        assert!(matches!(control.recv().await, Some(Control::Keyframe)));
        let repeated = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("keyframe recovery must replay unchanged codec configuration")
            .unwrap()
            .unwrap();
        assert_eq!(repeated.into_data().as_ref(), config);
        assert!(h.publish(Frame {
            data: vec![0, 0, 0, 1, 101, 2],
            pts_us: 223456,
            key: true
        }));
        let recovered = ws.next().await.unwrap().unwrap().into_data();
        assert_eq!(u64::from_le_bytes(recovered[3..11].try_into().unwrap()), 223456);
        ws.send(WsMessage::Text(
            r#"{"type":"touch","contacts":[{"id":0,"x":1.0,"y":1.0,"down":true}]}"#.into(),
        ))
        .await
        .unwrap();
        match control.recv().await.unwrap() {
            Control::Touch(b) => {
                assert_eq!(u16::from_le_bytes([b[2], b[3]]), 799);
                assert_eq!(u16::from_le_bytes([b[4], b[5]]), 479)
            }
            _ => panic!(),
        }
        ws.send(WsMessage::Text(r#"{"type":"home"}"#.into())).await.unwrap();
        assert!(matches!(tokio::time::timeout(Duration::from_secs(2), control.recv()).await.unwrap(), Some(Control::Home)));
        for (message,expected) in [
            (r#"{"type":"knob","knob":{"select":true,"home":false,"back":false,"x":-127,"y":127,"wheel":0}}"#, "knob"),
            (r#"{"type":"media","media":4}"#, "media"), (r#"{"type":"siri"}"#, "siri")] {
            ws.send(WsMessage::Text(message.into())).await.unwrap();
            let event=tokio::time::timeout(Duration::from_secs(2),control.recv()).await.unwrap().unwrap();
            assert!(match (expected,event) {("knob",Control::Knob([1,129,127,0]))|("media",Control::Media(4))|("siri",Control::Siri)=>true,_=>false});
        }
        ws.close(None).await.unwrap();
        let event = tokio::time::timeout(Duration::from_secs(2), control.recv())
            .await
            .unwrap();
        assert!(matches!(event, Some(Control::ViewerDisconnected)));
        task.abort();
    }
    #[tokio::test]
    async fn missing_tls_never_falls_back() {
        let c = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            origin: "https://test.invalid".into(),
            tls_certificate: "/nonexistent/f50-test-cert".into(),
            tls_private_key: "/nonexistent/f50-test-key".into(),
            access_token: "synthetic-token-for-tests-only".into(),
        };
        let (h, _) = Hub::new(c.access_token.clone(), c.origin.clone());
        assert!(run(c, h, Handle::new()).await.is_err());
    }
}

pub mod receiver;

pub async fn validate_tls_files(
    config: &Config,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    config.validate()?;
    axum_server::tls_rustls::RustlsConfig::from_pem_file(
        &config.tls_certificate,
        &config.tls_private_key,
    )
    .await?;
    Ok(())
}

pub mod settings;

pub mod discovery;

pub mod pairing_store;

#[cfg(feature = "resource-probe")]
impl Hub {
    pub fn resource_snapshot(&self) -> serde_json::Value {
        let a = self.audio.lock().unwrap();
        serde_json::json!({"video_bytes":self.queue.lock().unwrap().bytes(),"audio_bytes":a.bytes,"audio_packets":a.packets.len(),"viewer":self.viewer.load(Ordering::Acquire),"control_free":self.control.capacity()})
    }
}

pub mod phones;
