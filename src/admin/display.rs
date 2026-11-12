use super::{Admin, display_size, fit_box};
use super::preferences::size_mm;
use f50_server::diag::diag;
use std::time::{Duration, Instant};

impl Admin {
    /// Display declaration in physical pixels, with independent settings and explicit device caps.
    pub fn spec_for(&self, screen: &f50_server::clients::ScreenPrefs, window: Option<f50_server::clients::Window>, policy: &crate::policy::Policy) -> f50_server::DisplaySpec {
        let (mw, mh) = policy.max_box();
        let (mw, mh) = (mw.min(f50_core::info::MAX_WIDTH as u32) as f64, mh.min(f50_core::info::MAX_HEIGHT as u32) as f64);
        let scale = if screen.quality == "sharp" || (screen.quality=="custom" && screen.resolution=="1.5x") { 1.5 } else { 1.0 };
        let region = window.filter(|w| w.valid() && w.rw >= 160 && w.rh >= 120).map(|w| (w.rw as f64 * w.dpr * scale, w.rh as f64 * w.dpr * scale));
        let (mut rw, mut rh) = region.unwrap_or((self.display.0 as f64, self.display.1 as f64));
        let custom = screen.quality == "custom";
        if custom {
            if let Some((w,h))=screen.exact_resolution() { rw=w as f64; rh=h as f64; }
            let target = match screen.resolution.as_str() {
                "720p" => Some((1280.0,720.0)), "1080p" => Some((1920.0,1080.0)),
                "1440p" => Some((2560.0,1440.0)), "2160p" => Some((3840.0,2160.0)), _ => None,
            };
            if let Some((tw, th)) = target { let scale = (tw / rw).min(th / rh); rw *= scale; rh *= scale; }
        }
        // A too-narrow aspect may not fit at protocol minima; keep the user's cap.
        let fallback=(self.display.0.clamp(f50_core::info::MIN_WIDTH,mw as u16),self.display.1.clamp(f50_core::info::MIN_HEIGHT,mh as u16));
        let (width,height)=fit_box(rw,rh,mw,mh).unwrap_or(fallback);
        let fps = if custom { screen.fps } else if screen.quality == "smooth" { 30 } else { 60 }.min(policy.max_fps);
        // Presets retain H.264 compatibility; an explicit custom preference offers HEVC.
        f50_server::DisplaySpec { width, height, fps, width_mm: size_mm(display_size(&screen.ui_size)), hevc: custom && screen.codec == "hevc", microphone: screen.call == "web", right_hand_drive: screen.right_hand_drive }
    }
    fn wanted_display(&self) -> Option<f50_server::DisplaySpec> {
        let policy = self.policy();
        self.display_for(&policy, self.viewer().as_ref())
    }
    pub(super) fn display_for(&self, policy: &crate::policy::Policy, viewer: Option<&f50_server::clients::Client>) -> Option<f50_server::DisplaySpec> {
        if !policy.auto_downgrade || self.stopped.load(std::sync::atomic::Ordering::Acquire) { self.auto_fps.lock().unwrap().sync(None); }
        let mut spec = match viewer {
            Some(v) => {
                self.hub.set_night_mode(v.prefs.night);
                self.spec_for(&v.prefs, v.window, &policy)
            }
            None if self.hub.display_spec().is_none() => self.spec_for(&policy.defaults, None, &policy),
            None => return None,
        };
        if let Some(viewer) = viewer {
            let context = self.auto_context(policy, viewer);
            let mut auto = self.auto_fps.lock().unwrap();
            auto.sync(context);
            if let Some(limit) = auto.limit() { spec.fps = spec.fps.min(limit); }
        }
        Some(spec)
    }
    pub(super) fn auto_context(&self, policy: &crate::policy::Policy, viewer: &f50_server::clients::Client) -> Option<crate::auto_fps::Context> {
        if !policy.auto_downgrade || self.stopped.load(std::sync::atomic::Ordering::Acquire) { return None; }
        let phone = self.hub.phones.snapshot();
        if !phone.admission { return None; }
        Some(crate::auto_fps::Context::new(phone.epoch, phone.device?, viewer.id.clone(), viewer.prefs.clone(), policy.max_fps, policy.max_res.clone()))
    }
    pub(crate) fn auto_fps_limit(&self) -> Option<u8> {
        if !self.policy().auto_downgrade || self.stopped.load(std::sync::atomic::Ordering::Acquire) || !self.hub.display_spec().is_some_and(|s| s.fps == 30) { return None; }
        let phone = self.hub.phones.snapshot();
        phone.admission.then(|| self.auto_fps.lock().unwrap().limit_for_phone(phone.epoch)).flatten()
    }
    pub(super) fn observe_auto_fps(&self) {
        let policy = self.policy();
        if !policy.auto_downgrade || self.stopped.load(std::sync::atomic::Ordering::Acquire) {
            self.auto_fps.lock().unwrap().sync(None); return;
        }
        let generation = (self.hub.viewer_generation(), self.hub.media_generation());
        let Some(viewer) = self.viewer() else {
            self.auto_fps.lock().unwrap().observe(Instant::now(), generation, false, 0.0, None); return;
        };
        let sample = diag().viewer_sample_for(&viewer.id, generation.0);
        let valid = self.hub.has_phone_session() && self.hub.phone_verified.load(std::sync::atomic::Ordering::Acquire)
            && self.hub.display_spec().is_some_and(|s| s.fps > 30)
            && sample.as_ref().is_some_and(|(age, report)| *age <= Duration::from_millis(2500) && report.visible == Some(true) && report.fps.is_some_and(|fps| fps.is_finite() && fps > 0.0));
        let source_fps = diag().metrics()["server"]["fps_in"].as_f64().unwrap_or(0.0);
        let ping = sample.and_then(|(age, report)| report.ping.filter(|ping| age.saturating_add(Duration::from_millis(ping.age_ms)) <= Duration::from_millis(2500)).map(|ping| (ping.seq, ping.rtt_ms)));
        let context = self.auto_context(&policy, &viewer);
        let changed = {
            let mut auto = self.auto_fps.lock().unwrap();
            auto.sync(context.clone());
            auto.observe(Instant::now(), generation, valid, source_fps, ping)
        };
        if changed { if let Some(context) = context { self.commit_auto_fps(context, &viewer, generation); } }
    }
    fn commit_auto_fps(&self, expected: crate::auto_fps::Context, viewer: &f50_server::clients::Client, generation: (u64, u64)) {
        use std::sync::atomic::Ordering;
        if self.reinviting.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            self.reject_auto_fps(&expected); return;
        }
        let applied = self.hub.with_display_context(expected.epoch, &viewer.id, generation.0, generation.1, || {
            let policy = self.policy();
            if !policy.auto_downgrade || self.stopped.load(Ordering::Acquire) { return false; }
            // list() is an in-memory snapshot; get() may persist last-use metadata.
            let Some(current) = self.hub.clients.list().into_iter().find(|v| v.id == viewer.id) else { return false; };
            let context = crate::auto_fps::Context::new(expected.epoch, expected.phone.clone(), current.id.clone(), current.prefs.clone(), policy.max_fps, policy.max_res.clone());
            if context != expected { return false; }
            let mut auto = self.auto_fps.lock().unwrap();
            if !auto.pending(&expected) { return false; }
            if !self.set_auto_spec(self.spec_for(&current.prefs, current.window, &policy)) { return false; }
            auto.accept(&expected)
        }).unwrap_or(false);
        if applied {
            diag().event_detail("info", "carplay", "持续抖动，已请求 30 帧", "仅本次手机与屏幕设置生效；实际输入和绘制帧率需查看诊断。");
            self.reinvite_task("持续抖动，自动请求 30 帧", expected.epoch);
        } else {
            self.reinviting.store(false, Ordering::Release); self.reject_auto_fps(&expected);
        }
    }
    fn reject_auto_fps(&self, context: &crate::auto_fps::Context) {
        if self.auto_fps.lock().unwrap().reject(context) {
            diag().event_detail("warn", "carplay", "自动降帧请求未受理，保留原画面请求", "通道忙或会话/设置已变化；本次不自动重试。需要时关闭再启用自动降帧。");
        }
    }
    /// Commit only after a bounded EndSession command is accepted; a full queue
    /// must not leave a cached 30 fps declaration that was never requested.
    pub(super) fn set_auto_spec(&self, mut spec: f50_server::DisplaySpec) -> bool {
        let Some(previous) = self.hub.display_spec() else { return false; };
        if previous.fps <= 30 { return false; }
        spec.width = previous.width; spec.height = previous.height; spec.fps = spec.fps.min(30);
        self.hub.set_display_spec(spec);
        if self.hub.end_session() { true } else { self.hub.set_display_spec(previous); false }
    }
    /// Ordinary viewport changes scale the existing stream; explicit settings may renegotiate.
    pub fn apply_display(&self, restart: bool) {
        if let Some(spec) = self.wanted_display() { self.commit_display(spec, restart); }
    }
    /// Use the latest screen declaration immediately before starting a new phone session.
    pub fn prepare_display(&self) {
        if let Some(spec) = self.wanted_display() { self.hub.set_display_spec(spec); }
    }
    pub(super) fn commit_display(&self, mut spec: f50_server::DisplaySpec, restart: bool) {
        let live = self.hub.has_phone_session();
        if live && !restart {
            if let Some(current) = self.hub.display_spec() {
                spec.width = current.width; spec.height = current.height;
            }
        }
        if !self.hub.set_display_spec(spec) || (!restart && !live) { return; }
        self.reinvite(&format!("画面协商为 {}×{}@{}，{}", spec.width, spec.height, spec.fps, if spec.microphone { "通话走车机浏览器麦克风" } else { "不使用浏览器麦克风" }));
    }
}
