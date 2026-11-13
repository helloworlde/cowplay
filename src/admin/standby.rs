//! Demand-driven phone lifecycle. HTTP activity is not a viewing screen.
use super::Admin;
use f50_server::diag::diag;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::{sync::watch, time::Instant};

pub(crate) const IDLE: Duration = Duration::from_secs(60);
const ATTEMPT: Duration = Duration::from_secs(35);

#[derive(Default)]
struct State {
    armed: bool,
    requested: Option<(u64, String)>,
    next: Option<Instant>,
    failures: u32,
    viewer: u64,
    selection: u64,
    idle_done: Option<u64>,
    idle_retry: Option<Instant>,
}

pub(crate) struct Standby {
    state: Mutex<State>,
    manual: AtomicUsize,
    changes: watch::Sender<u64>,
    /// Only this coordinator's paused selection may be resumed without a user action.
    owned: AtomicU64,
}
impl Default for Standby {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            manual: AtomicUsize::new(0),
            changes: watch::channel(0).0,
            owned: AtomicU64::new(0),
        }
    }
}
pub(crate) struct Manual {
    automatic: Arc<Standby>,
    revision: u64,
}
impl Manual {
    pub(crate) fn selected(&self, a: &Admin, target: &str) {
        let mut s = self.automatic.state.lock().unwrap();
        if *self.automatic.changes.borrow() == self.revision
            && !a.user_stopped()
            && a.manager.selected_phone().as_deref() == Some(target)
        {
            self.automatic
                .owned
                .store(a.hub.phones.epoch(), Ordering::Release);
            s.selection = a.hub.phones.epoch();
            s.viewer = a.hub.viewer_generation();
            s.failures = 0;
            s.next = Some(Instant::now() + Duration::from_secs(15));
        }
    }
}
impl Drop for Manual {
    fn drop(&mut self) {
        self.automatic.manual.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Standby {
    pub(crate) fn reinvite_current(&self, a: &Admin, epoch: u64) {
        let _s = self.state.lock().unwrap();
        if self.allowed(a, a.hub.viewer_generation()) && a.hub.phones.is_current(epoch) {
            a.reinvite("车机请求恢复当前 CarPlay 会话");
        }
    }
    /// Publish user intent before any asynchronous lookup or spawned phone operation.
    pub(crate) fn manual(self: &Arc<Self>) -> Manual {
        self.manual.fetch_add(1, Ordering::AcqRel);
        let mut s = self.state.lock().unwrap();
        self.changes.send_modify(|v| *v = v.wrapping_add(1));
        s.armed = true;
        s.requested = None;
        s.next = None;
        s.failures = 0;
        s.idle_retry = None;
        Manual {
            automatic: self.clone(),
            revision: *self.changes.borrow(),
        }
    }
    /// Repeated page requests do not advance a retry deadline or replace a selected phone.
    pub(crate) fn request(&self, a: &Admin, target: String, epoch: u64) {
        if a.user_stopped()
            || !a.hub.phones.is_current(epoch)
            || self.manual.load(Ordering::Acquire) != 0
        {
            return;
        }
        let mut s = self.state.lock().unwrap();
        if !s.armed {
            s.requested = Some((epoch, target));
        }
        s.armed = true;
    }
    fn allowed(&self, a: &Admin, viewer: u64) -> bool {
        self.manual.load(Ordering::Acquire) == 0
            && !a.user_stopped()
            && a.hub.viewer_active()
            && a.hub.viewer_generation() == viewer
            && !a.manager.pairing_open()
            && a.manager.fresh_pairing().is_none()
    }
    fn owns(&self, epoch: u64) -> bool {
        epoch != 0 && self.owned.load(Ordering::Acquire) == epoch
    }
}

impl Admin {
    pub(crate) async fn run_standby(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut viewers = self.hub.subscribe_viewer();
        let mut every = tokio::time::interval(Duration::from_secs(1));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *stop.borrow() {
                return;
            }
            tokio::select! {
                biased;
                _ = stop.changed() => return,
                _ = viewers.changed() => {},
                _ = every.tick() => {},
            }
            // Dropping an in-flight operation on shutdown leaves cleanup to Manager::stop.
            tokio::select! {
                biased;
                _ = stop.changed() => return,
                _ = self.standby_step(Instant::now()) => {},
            }
        }
    }

    pub(crate) async fn standby_step(&self, now: Instant) {
        let auto = &self.automatic;
        let selection = self.hub.phones.snapshot();
        let viewing = self.hub.viewer_active();
        let viewer = self.hub.viewer_generation();
        let busy = auto.manual.load(Ordering::Acquire) != 0
            || self.user_stopped()
            || self.manager.pairing_open()
            || self.manager.fresh_pairing().is_some()
            || self.reinviting.load(Ordering::Acquire);
        let idle = {
            let mut s = auto.state.lock().unwrap();
            if s.selection != selection.epoch {
                s.selection = selection.epoch;
                s.failures = 0;
                s.next = Some(now + Duration::from_secs(15));
                if s.requested
                    .as_ref()
                    .is_some_and(|(e, _)| *e != selection.epoch)
                {
                    s.requested = None;
                }
            }
            if self.hub.has_phone_session() {
                s.armed = true;
            }
            if viewing {
                if s.viewer != viewer {
                    s.viewer = viewer;
                    s.next = None;
                    s.failures = 0;
                    s.idle_retry = None;
                }
                false
            } else {
                now.saturating_duration_since(self.hub.viewer_offline_since()) >= IDLE
            }
        };
        if busy {
            return;
        }
        if !viewing {
            let (cleanup_due, retry_ready) = {
                let s = auto.state.lock().unwrap();
                (
                    auto.owns(selection.epoch) && s.idle_done != Some(selection.epoch),
                    s.idle_retry.is_none_or(|next| now >= next),
                )
            };
            if idle
                && retry_ready
                && (selection.admission
                    || self.hub.has_phone_session()
                    || self.manager.status().rfcomm
                    || cleanup_due)
            {
                let Ok(_guard) = self.hub.phones.operation.try_lock() else {
                    return;
                };
                let (epoch, revision) = {
                    let _s = auto.state.lock().unwrap();
                    if self.hub.viewer_active()
                        || !self.hub.phones.is_current(selection.epoch)
                        || self.user_stopped()
                        || auto.manual.load(Ordering::Acquire) != 0
                        || self.manager.pairing_open()
                        || self.manager.fresh_pairing().is_some()
                        || self.reinviting.load(Ordering::Acquire)
                    {
                        return;
                    }
                    let Ok(epoch) = self.hub.pause_phone_if_current(selection.epoch) else {
                        return;
                    };
                    auto.owned.store(epoch, Ordering::Release);
                    (epoch, *auto.changes.borrow())
                };
                let result = crate::phone_flow::stop_locked(&self.manager, &self.hub, epoch).await;
                let mut s = auto.state.lock().unwrap();
                if *auto.changes.borrow() != revision || !self.hub.phones.is_current(epoch) {
                    return;
                }
                s.selection = epoch;
                if let Err(e) = result {
                    f50_server::diag_log!(Warn, "bt", "自动待机清理未完成：{e}");
                    s.idle_retry = Some(now.max(Instant::now()) + IDLE);
                } else {
                    s.idle_done = Some(epoch);
                    s.idle_retry = None;
                    diag().link_step(
                        "carplay",
                        "idle",
                        "车机已离线 60 秒，CarPlay 自动待机；配对保留，等待车机回来",
                    );
                }
            }
            return;
        }
        if self.hub.has_phone_session() || crate::console::handoff_now(self) {
            return;
        }
        let target = {
            let s = auto.state.lock().unwrap();
            if !s.armed || s.next.is_some_and(|next| now < next) {
                return;
            }
            s.requested
                .as_ref()
                .map(|(_, d)| d.clone())
                .or_else(|| self.manager.selected_phone())
        };
        let Some(target) = target else {
            return;
        };
        if !selection.admission && !auto.owns(selection.epoch) {
            return;
        }
        let mut changes = auto.changes.subscribe();
        let revision = *changes.borrow_and_update();
        let mut viewers = self.hub.subscribe_viewer();
        let connect = self.connect_viewing_phone(&target, selection.epoch, viewer);
        let result = tokio::select! {
            biased;
            _ = changes.changed() => None,
            _ = viewers.changed() => None,
            r = tokio::time::timeout(ATTEMPT, connect) => Some(r),
        };
        // An explicit selection owns its cleanup. Otherwise cancel the remote pending request,
        // not just the local future (BlueZ can still deliver a late RFCOMM descriptor).
        if auto.manual.load(Ordering::Acquire) == 0 {
            let epoch = auto.owned.load(Ordering::Acquire);
            if self.hub.phones.is_current(epoch)
                && !self.hub.has_phone_session()
                && !self.manager.status().rfcomm
            {
                if let Ok(_guard) = self.hub.phones.operation.try_lock() {
                    if self.hub.phones.is_current(epoch) {
                        let _ =
                            crate::phone_flow::stop_locked(&self.manager, &self.hub, epoch).await;
                    }
                }
            }
        }
        if *changes.borrow() != revision {
            return;
        }
        let mut s = auto.state.lock().unwrap();
        s.requested = None;
        s.selection = self.hub.phones.epoch();
        s.failures = s.failures.saturating_add(1);
        let delay = Duration::from_secs(match s.failures {
            1 => 15,
            2 => 30,
            _ => 60,
        });
        s.next = Some(now.max(Instant::now()) + delay);
        if let Some(r) = result {
            if !matches!(r, Ok(Ok(()))) {
                diag().link_step(
                    "bluetooth",
                    "idle",
                    format!(
                        "暂未连接 iPhone，{} 秒后再尝试；可手动选择手机",
                        delay.as_secs()
                    ),
                );
            }
        }
    }

    async fn connect_viewing_phone(
        &self,
        target: &str,
        expected: u64,
        viewer: u64,
    ) -> crate::RuntimeResult<()> {
        // Automation yields immediately to a user operation, including one validating a phone.
        let _guard = self
            .hub
            .phones
            .operation
            .try_lock()
            .map_err(|_| "手机操作正在进行")?;
        if !self.automatic.allowed(self, viewer) || !self.hub.phones.is_current(expected) {
            return Ok(());
        }
        self.manager.validate_phone(target).await?;
        if !self.automatic.allowed(self, viewer) || !self.hub.phones.is_current(expected) {
            return Ok(());
        }
        let epoch = {
            let _s = self.automatic.state.lock().unwrap();
            if !self.automatic.allowed(self, viewer) || !self.hub.phones.is_current(expected) {
                return Ok(());
            }
            let epoch = self.hub.pause_phone_if_current(expected)?;
            self.automatic.owned.store(epoch, Ordering::Release);
            epoch
        };
        crate::phone_flow::stop_locked(&self.manager, &self.hub, epoch).await?;
        if !self.automatic.allowed(self, viewer) {
            return Ok(());
        }
        let candidate = self.hub.phones.prepare(target)?;
        self.hub.phones.select(candidate, epoch, || {
            self.manager.select_phone(target, epoch)
        })?;
        self.prepare_display();
        diag().link_step("bluetooth", "active", "车机在线，正在连接所选 iPhone…");
        // One bounded invitation; the coordinator owns backoff and never queues parallel retries.
        crate::phone_flow::invite_locked(&self.manager, &self.hub, epoch).await
    }
}

#[cfg(test)]
mod tests;
