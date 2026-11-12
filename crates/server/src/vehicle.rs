//! Shared vehicle readings, freshness and availability for iAP2 and Ultra transports.
use f50_core::vehicle::{Category, ClimateCommand, ClimateState, Telemetry, Update, Values};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Clone, Default, Debug, PartialEq)]
pub struct ClimateSnapshot {
    pub context: u64,
    pub authorized: bool,
    pub state: Option<ClimateState>,
    pub received_ms: Option<u64>,
}
impl ClimateSnapshot {
    pub fn current(&self) -> Option<&ClimateState> {
        self.state.as_ref().filter(|s| self.authorized && self.received_ms.is_some_and(|at| s.fresh(at, now_ms())))
    }
}
pub struct ClimateRequest {
    pub context: u64,
    pub command: Option<ClimateCommand>,
    pub grant: watch::Receiver<Option<u64>>,
    pub reply: oneshot::Sender<bool>,
}
impl ClimateRequest {
    pub fn valid(&self, context: u64) -> bool {
        self.context == context && *self.grant.borrow() == Some(self.context) && !self.grant.has_changed().unwrap_or(true) && !self.reply.is_closed()
    }
}
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Snapshot {
    pub enabled: bool,
    pub connected: bool,
    pub phase: &'static str,
    pub telemetry: Telemetry,
    pub values: Values,
    pub updated_ms: u64,
}
struct State {
    snapshot: Snapshot,
    charge_current: bool,
    tires_current: bool,
    paused: [bool; 2],
}
pub struct Feed {
    state: Mutex<State>,
    changed: watch::Sender<Snapshot>,
    retry: watch::Sender<u64>,
    climate: watch::Sender<ClimateSnapshot>,
    climate_requests: mpsc::Sender<ClimateRequest>,
    climate_receiver: Mutex<Option<mpsc::Receiver<ClimateRequest>>>,
}
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
impl Feed {
    pub fn new() -> Arc<Self> {
        let snapshot = Snapshot {
            enabled: false,
            connected: false,
            phase: "unconfigured",
            telemetry: Telemetry::default(),
            values: Values::default(),
            updated_ms: now_ms(),
        };
        let (changed, _) = watch::channel(snapshot.clone());
        let (retry, _) = watch::channel(0);
        let (climate, _) = watch::channel(ClimateSnapshot::default());
        let (climate_requests, climate_receiver) = mpsc::channel(1);
        Arc::new(Self {
            state: Mutex::new(State {
                snapshot,
                charge_current: false,
                tires_current: false,
                paused: [false; 2],
            }),
            changed,
            retry,
            climate,
            climate_requests,
            climate_receiver: Mutex::new(Some(climate_receiver)),
        })
    }
    pub fn climate(&self) -> ClimateSnapshot { self.climate.borrow().clone() }
    pub fn subscribe_climate(&self) -> watch::Receiver<ClimateSnapshot> { self.climate.subscribe() }
    pub fn publish_climate(&self, authorized: bool, state: Option<ClimateState>, received_ms: Option<u64>) {
        self.climate.send_if_modified(|s| {
            if s.authorized == authorized && s.state == state && s.received_ms == received_ms { return false; }
            s.authorized = authorized; s.state = state; s.received_ms = received_ms; true
        });
    }
    pub fn reset_climate(&self) {
        self.climate.send_modify(|s| *s = ClimateSnapshot { context: s.context.wrapping_add(1), ..Default::default() });
    }
    pub fn take_climate_requests(&self) -> Option<mpsc::Receiver<ClimateRequest>> { self.climate_receiver.lock().unwrap().take() }
    pub fn request_climate(&self, context: u64, command: Option<ClimateCommand>, grant: watch::Receiver<Option<u64>>) -> Option<oneshot::Receiver<bool>> {
        let snapshot = self.climate();
        if snapshot.context != context || *grant.borrow() != Some(context) || grant.has_changed().unwrap_or(true) || !snapshot.authorized || (command.is_some() && snapshot.current().is_none()) { return None; }
        let (reply, receiver) = oneshot::channel();
        self.climate_requests.try_send(ClimateRequest { context, command, grant, reply }).ok()?;
        Some(receiver)
    }
    pub fn snapshot(&self) -> Snapshot {
        self.state.lock().unwrap().snapshot.clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.changed.subscribe()
    }
    pub fn retries(&self) -> watch::Receiver<u64> {
        self.retry.subscribe()
    }
    pub fn request_retry(&self) -> bool {
        self.retry.send_modify(|n| *n = n.wrapping_add(1));
        true
    }
    /// A configuration reload starts a new vehicle context. Never borrow another VIN's readings.
    pub fn reset(&self) {
        self.modify(|s| {
            s.snapshot.telemetry = Telemetry::default();
            s.paused = [false; 2];
            s.charge_current = false;
            s.tires_current = false;
        });
    }
    pub fn status(&self, enabled: bool, connected: bool, phase: &'static str) {
        self.modify(|s| {
            if !connected || !enabled {
                s.charge_current = false;
                s.tires_current = false;
            }
            s.snapshot.enabled = enabled;
            s.snapshot.connected = connected;
            s.snapshot.phase = phase;
        });
    }
    pub fn apply(&self, update: Update) {
        self.modify(|s| {
            match &update {
                Update::Charge { .. } => s.charge_current = s.snapshot.connected,
                Update::Tires(_) => s.tires_current = s.snapshot.connected,
            }
            s.snapshot.telemetry.apply(update);
            s.snapshot.phase = "reading";
        });
    }
    /// Failed categories must not retain values while another category is still healthy.
    pub fn read_failed(&self, category: Category, paused: bool) {
        self.modify(|s| {
            match category {
                Category::Charge => { s.charge_current = false; s.snapshot.telemetry.soc_pct = None; s.snapshot.telemetry.range_km = None; s.paused[0] = paused; }
                Category::Tires => { s.tires_current = false; s.snapshot.telemetry.tires_kpa = [None;4]; s.paused[1] = paused; }
            }
        });
    }
    pub fn tick(&self) {
        self.modify(|_| {});
    }
    fn modify(&self, change: impl FnOnce(&mut State)) {
        let mut state = self.state.lock().unwrap();
        let before = state.snapshot.clone();
        change(&mut state);
        let reading = matches!(state.snapshot.phase, "reading" | "charge_paused" | "tires_paused" | "reads_paused");
        if reading {
            state.snapshot.phase = match state.paused {
                [true,true] => "reads_paused", [true,false] => "charge_paused", [false,true] => "tires_paused", _ => "reading",
            };
        }
        let mut values = if state.snapshot.connected && reading {
            state.snapshot.telemetry.fresh(now_ms())
        } else {
            Values::default()
        };
        if !state.charge_current {
            values.soc_pct = None;
            values.range_km = None;
        }
        if !state.tires_current {
            values.tires_kpa = [None; 4];
        }
        state.snapshot.values = values;
        if state.snapshot.enabled != before.enabled
            || state.snapshot.connected != before.connected
            || state.snapshot.phase != before.phase
            || state.snapshot.values != before.values
            || state.snapshot.telemetry != before.telemetry
        {
            state.snapshot.updated_ms = now_ms();
            self.changed.send_replace(state.snapshot.clone());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use f50_core::vehicle::Reading;
    fn reading(value: f64) -> Reading {
        let now = now_ms();
        Reading {
            value,
            received_ms: now,
            sample_received_ms: now,
            sampled_ms: Some(now),
            sampled_time_trusted: true,
        }
    }
    #[test]
    fn reconnect_requires_new_read_for_each_category_and_reload_cannot_borrow_another_vehicle() {
        let feed = Feed::new();
        feed.status(true, true, "reading");
        feed.apply(Update::Charge {
            soc_pct: Some(reading(68.0)),
            range_km: Some(reading(320.0)),
        });
        feed.apply(Update::Tires([Some(reading(250.0)); 4]));
        assert_eq!(feed.snapshot().values.soc_pct, Some(68.0));
        feed.status(true, false, "disconnected");
        feed.status(true, true, "authorizing");
        feed.status(true, true, "reading");
        assert_eq!(feed.snapshot().values, Values::default());
        feed.apply(Update::Charge {
            soc_pct: Some(reading(69.0)),
            range_km: Some(reading(321.0)),
        });
        assert_eq!(feed.snapshot().values.soc_pct, Some(69.0));
        assert_eq!(feed.snapshot().values.tires_kpa, [None; 4]);
        feed.apply(Update::Tires([Some(reading(251.0)); 4]));
        assert_eq!(feed.snapshot().values.tires_kpa, [Some(251.0); 4]);
        feed.reset();
        assert_eq!(feed.snapshot().telemetry, Telemetry::default());
        assert_eq!(feed.snapshot().values, Values::default());
    }
}
