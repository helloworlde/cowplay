//! Opt-in, one-way frame-rate reduction for the current phone and screen preferences.
use f50_server::clients::ScreenPrefs;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Context {
    pub(crate) epoch: u64,
    pub(crate) phone: String,
    screen: String,
    prefs: ScreenPrefs,
    max_fps: u8,
    max_res: String,
}

impl Context {
    pub(crate) fn new(epoch: u64, phone: String, screen: String, mut prefs: ScreenPrefs, max_fps: u8, max_res: String) -> Self {
        // Cosmetic controls must not restore 60 fps or restart a congestion decision.
        prefs.rail_side.clear(); prefs.night = false; prefs.debug = false;
        Self { epoch, phone, screen, prefs, max_fps, max_res }
    }
}

#[derive(Default)]
pub(crate) struct AutoFps {
    context: Option<Context>,
    bad_since: Option<Instant>,
    last_sample: Option<Instant>,
    last_ping: Option<(u64, f64)>,
    generation: Option<(u64, u64)>,
    limited: bool,
    requested: bool,
    suppressed: bool,
}

impl AutoFps {
    pub(crate) fn sync(&mut self, context: Option<Context>) {
        if self.context != context {
            *self = Self { context, ..Self::default() };
        }
    }

    pub(crate) fn limit(&self) -> Option<u8> { self.limited.then_some(30) }
    pub(crate) fn pending(&self, context: &Context) -> bool { self.requested && self.context.as_ref() == Some(context) && !self.suppressed }
    pub(crate) fn accept(&mut self, context: &Context) -> bool {
        if !self.pending(context) { return false; }
        self.limited = true; true
    }
    pub(crate) fn reject(&mut self, context: &Context) -> bool {
        if self.context.as_ref() != Some(context) { return false; }
        self.limited = false; self.requested = false; self.suppressed = true; true
    }
    pub(crate) fn limit_for_phone(&self, epoch: u64) -> Option<u8> {
        self.context.as_ref().filter(|context| context.epoch == epoch).and_then(|_| self.limit())
    }

    /// All samples must belong to the active authenticated viewer channel. A reconnect
    /// resets the measurement window; an already chosen limit survives that reconnect.
    pub(crate) fn observe(&mut self, now: Instant, generation: (u64, u64), valid: bool, source_fps: f64, ping: Option<(u64, f64)>) -> bool {
        if self.requested || self.suppressed || self.context.is_none() { return false; }
        if self.generation != Some(generation) || self.last_sample.is_some_and(|at| now.duration_since(at) > Duration::from_millis(2500)) {
            self.bad_since = None;
            self.last_ping = None;
        }
        self.generation = Some(generation);
        // Diagnostic RTT/jitter are rolling averages: an old spike must not count
        // as five seconds of current jitter. Only distinct fresh pongs advance time.
        let Some((seq, rtt)) = ping.filter(|(_, rtt)| rtt.is_finite() && *rtt >= 0.0).filter(|_| valid && source_fps.is_finite() && source_fps > 30.0) else {
            self.bad_since = None;
            self.last_ping = None;
            return false;
        };
        if self.last_ping.is_some_and(|(previous, _)| previous == seq) { return false; }
        let previous = self.last_ping.replace((seq, rtt));
        self.last_sample = Some(now);
        if !previous.is_some_and(|(previous_seq, previous_rtt)| seq > previous_seq && (rtt - previous_rtt).abs() > 10.0) {
            self.bad_since = None;
            return false;
        }
        let since = *self.bad_since.get_or_insert(now);
        if now.duration_since(since) < Duration::from_secs(5) { return false; }
        self.requested = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(epoch: u64, phone: &str, screen: &str, prefs: ScreenPrefs) -> Context {
        Context::new(epoch, phone.into(), screen.into(), prefs, 60, "1080p".into())
    }
    fn start() -> (AutoFps, Instant) {
        let mut state = AutoFps::default();
        state.sync(Some(context(1, "synthetic-phone", "synthetic-screen", ScreenPrefs::default())));
        (state, Instant::now())
    }
    fn ping(seq: u64) -> Option<(u64, f64)> { Some((seq, if seq % 2 == 0 {10.0} else {40.0})) }
    fn sustain(state: &mut AutoFps, now: Instant, generation: u64) {
        for sec in 0..6 { assert!(!state.observe(now + Duration::from_secs(sec), (generation, 1), true, 60.0, ping(sec))); }
        assert!(state.observe(now + Duration::from_secs(6), (generation, 1), true, 60.0, ping(6)));
        assert_eq!(state.limit(), None, "a decision is not an accepted display request");
        assert!(state.accept(&state.context.clone().unwrap()));
    }
    #[test]
    fn five_seconds_of_fresh_high_jitter_caps_only_once_and_survives_planned_reconnect() {
        let (mut state, now) = start();
        sustain(&mut state, now, 1);
        assert_eq!(state.limit(), Some(30));
        assert_eq!(state.limit_for_phone(1), Some(30));assert_eq!(state.limit_for_phone(2),None);
        assert!(!state.observe(now + Duration::from_secs(7), (2, 1), false, 0.0, None));
        assert_eq!(state.limit(), Some(30));
        assert!(!state.observe(now + Duration::from_secs(30), (2, 1), true, 60.0, ping(30)));
    }
    #[test]
    fn stale_hidden_or_low_rate_samples_break_the_window() {
        for (valid, fps, sample) in [(false,60.0,ping(4)),(true,0.0,ping(4)),(true,30.0,ping(4)),(true,60.0,None),(true,60.0,Some((4,f64::NAN))),(true,f64::INFINITY,ping(4))] {
            let (mut state, now) = start();
            for sec in 0..4 { assert!(!state.observe(now + Duration::from_secs(sec),(1,1),true,60.0,ping(sec))); }
            assert!(!state.observe(now + Duration::from_secs(4),(1,1),valid,fps,sample));
            assert!(!state.observe(now + Duration::from_secs(5),(1,1),true,60.0,ping(5)));
            assert_eq!(state.limit(),None);
        }
    }
    #[test]
    fn an_unobserved_gap_or_replacement_viewer_channel_restarts_measurement() {
        let (mut state, now) = start();
        for sec in 0..5 { assert!(!state.observe(now + Duration::from_secs(sec),(1,1),true,60.0,ping(sec))); }
        assert!(!state.observe(now + Duration::from_secs(5),(2,1),true,60.0,ping(5)));
        assert!(!state.observe(now + Duration::from_secs(11),(2,1),true,60.0,ping(11)));
        assert_eq!(state.limit(),None);
    }
    #[test]
    fn a_single_old_spike_normal_rtt_or_repeated_report_cannot_trigger_a_reconnect() {
        for repeat in [false,true] {
            let (mut state, now) = start();
            for sec in 0..20 {
                let sample = if repeat {ping(sec.min(4))} else {Some((sec, if sec == 1 {610.0} else {10.0}))};
                assert!(!state.observe(now + Duration::from_secs(sec),(1,1),true,60.0,sample));
            }
            assert_eq!(state.limit(),None);
        }
    }
    #[test]
    fn media_reset_restarts_measurement_and_a_rejected_request_needs_an_explicit_context_change() {
        let (mut state, now) = start();
        for sec in 0..6 { assert!(!state.observe(now + Duration::from_secs(sec), (1,1), true, 60.0, ping(sec))); }
        assert!(!state.observe(now + Duration::from_secs(6), (1,2), true, 60.0, ping(6)));
        for sec in 7..12 { assert!(!state.observe(now + Duration::from_secs(sec), (1,2), true, 60.0, ping(sec))); }
        assert!(state.observe(now + Duration::from_secs(12), (1,2), true, 60.0, ping(12)));
        let context = state.context.clone().unwrap();
        assert!(state.reject(&context)); assert_eq!(state.limit(), None);
        for sec in 13..30 { assert!(!state.observe(now + Duration::from_secs(sec), (1,2), true, 60.0, ping(sec))); }
        state.sync(None); state.sync(Some(context));
        sustain(&mut state, now + Duration::from_secs(30), 2);
    }
    #[test]
    fn stop_phone_switch_screen_change_and_real_picture_changes_restore_the_request() {
        let mut changed = ScreenPrefs::default(); changed.quality = "sharp".into();
        for next in [None,Some(context(2,"synthetic-phone","synthetic-screen",ScreenPrefs::default())),Some(context(1,"other-phone","synthetic-screen",ScreenPrefs::default())),Some(context(1,"synthetic-phone","other-screen",ScreenPrefs::default())),Some(context(1,"synthetic-phone","synthetic-screen",changed))] {
            let (mut state, now) = start(); sustain(&mut state,now,1);state.sync(next);assert_eq!(state.limit(),None);
        }
    }
    #[test]
    fn debug_night_and_rail_controls_do_not_restore_sixty_fps() {
        let (mut state, now) = start(); sustain(&mut state,now,1);
        let mut prefs = ScreenPrefs::default(); prefs.debug=true;prefs.night=true;prefs.rail_side="right".into();
        state.sync(Some(context(1,"synthetic-phone","synthetic-screen",prefs)));
        assert_eq!(state.limit(),Some(30));
    }
}
