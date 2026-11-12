use super::{
    Result,
    worker::{self, Config},
};
use f50_core::tesla::{ClimateCommand, ClimateState, EnrollmentPermission, Identity};
use f50_server::vehicle::{Feed, Snapshot, now_ms};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};

const PAIR_WINDOW: Duration = Duration::from_secs(90);
type Job = Pin<Box<dyn Future<Output = Result<Outcome>> + Send>>;
enum Outcome {
    Done,
    Climate(Option<ClimateState>),
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    vin: String,
    private_key: PathBuf,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    control_private_key: Option<PathBuf>,
}
fn enabled_by_default() -> bool {
    true
}
impl Saved {
    fn config(&self) -> Config {
        Config {
            vin: self.vin.clone(),
            private_key: self.private_key.clone(),
        }
    }
}

#[derive(Clone, serde::Serialize)]
pub struct Status {
    pub configured: bool,
    pub enabled: bool,
    pub connected: bool,
    pub phase: &'static str,
    pub pairing: &'static str,
    pub pairing_deadline_ms: Option<u64>,
    pub vin_masked: Option<String>,
    pub key_ready: bool,
    pub public_key_fingerprint: Option<String>,
    pub read_only: bool,
    pub permission: &'static str,
    pub climate_authorized: bool,
    pub vehicle: Snapshot,
    pub climate: ClimateStatus,
}
#[derive(Clone, serde::Serialize)]
pub struct ClimateStatus {
    pub authorized: bool,
    pub permission: &'static str,
    pub key_ready: bool,
    pub public_key_fingerprint: Option<String>,
    pub phase: &'static str,
    pub state: Option<ClimateState>,
    pub received_ms: Option<u64>,
    pub fresh: bool,
    pub last_command_status: &'static str,
}
impl Default for ClimateStatus {
    fn default() -> Self {
        Self {
            authorized: false,
            permission: "driver",
            key_ready: false,
            public_key_fingerprint: None,
            phase: "unconfigured",
            state: None,
            received_ms: None,
            fresh: false,
            last_command_status: "idle",
        }
    }
}
impl ClimateStatus {
    fn fresh(&self, now: u64) -> bool {
        self.state.as_ref().zip(self.received_ms).is_some_and(|(state, at)| state.fresh(at, now))
    }
    fn clear_reading(&mut self) {
        self.state = None;
        self.received_ms = None;
        self.fresh = false;
    }
}
struct State {
    saved: Option<Saved>,
    phase: Option<&'static str>,
    pairing: &'static str,
    deadline: Option<u64>,
    fingerprint: Option<String>,
    disconnected: bool,
    cleanup_failed: bool,
    climate: ClimateStatus,
}

pub enum Command {
    Configure { vin: Option<String>, enabled: bool },
    Connect,
    Disconnect,
    Pair,
    Cancel,
    ClimatePair,
    ClimateQuery,
    ClimateCommand(ClimateCommand),
}
struct Request {
    command: Command,
    reply: oneshot::Sender<std::result::Result<Status, String>>,
}
#[derive(Clone, Copy, PartialEq)]
enum Operation {
    Poll,
    Pair,
    ClimatePair,
    ClimateQuery,
    ClimateCommand(ClimateCommand),
}
struct Active {
    operation: Operation,
    cancel: watch::Sender<bool>,
    task: Job,
    deadline: Option<tokio::time::Instant>,
}
impl Drop for Active {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
    }
}

pub struct Manager {
    directory: PathBuf,
    legacy: PathBuf,
    feed: Arc<Feed>,
    state: Mutex<State>,
    sender: mpsc::Sender<Request>,
    receiver: Mutex<Option<mpsc::Receiver<Request>>>,
}
impl Manager {
    pub fn new(state_dir: &Path, feed: Arc<Feed>) -> Arc<Self> {
        let directory = state_dir.join("tesla");
        let legacy = state_dir.join("tesla-ble.json");
        let loaded = load(&directory.join("config.json"), &legacy);
        let phase = if loaded.is_err() {
            Some("configuration_error")
        } else {
            None
        };
        let saved = loaded.ok().flatten();
        let fingerprint = saved
            .as_ref()
            .and_then(|s| fingerprint(&s.private_key).ok());
        let climate = climate_status(saved.as_ref());
        let (sender, receiver) = mpsc::channel(8);
        Arc::new(Self {
            directory,
            legacy,
            feed,
            state: Mutex::new(State {
                saved,
                phase,
                pairing: "idle",
                deadline: None,
                fingerprint,
                disconnected: false,
                cleanup_failed: false,
                climate,
            }),
            sender,
            receiver: Mutex::new(Some(receiver)),
        })
    }
    pub fn status(&self) -> Status {
        let s = self.state.lock().unwrap();
        let vehicle = self.feed.snapshot();
        let enabled = s.saved.as_ref().is_some_and(|c| c.enabled);
        let mut climate = s.climate.clone();
        climate.fresh = climate.fresh(now_ms());
        if !climate.fresh && climate.state.is_some() && climate.phase == "ready" {
            climate.phase = "stale";
        }
        Status {
            configured: s.saved.is_some(),
            enabled,
            connected: vehicle.connected && s.phase.is_none(),
            phase: s.phase.unwrap_or(vehicle.phase),
            pairing: s.pairing,
            pairing_deadline_ms: s.deadline,
            vin_masked: s
                .saved
                .as_ref()
                .map(|c| format!("{}••••••••••{}", &c.vin[..3], &c.vin[13..])),
            key_ready: s.fingerprint.is_some(),
            public_key_fingerprint: s.fingerprint.clone(),
            read_only: !climate.authorized,
            permission: "vehicle_monitor",
            climate_authorized: climate.authorized,
            vehicle,
            climate,
        }
    }
    pub async fn command(&self, command: Command) -> std::result::Result<Status, String> {
        if let Command::Configure { vin: Some(vin), .. } = &command {
            validate_vin(vin)?;
        }
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(Request { command, reply })
            .map_err(|_| "Tesla管理任务繁忙或服务已停止".to_owned())?;
        result.await.map_err(|_| "Tesla管理服务已停止".to_owned())?
    }
    pub async fn run(self: Arc<Self>, bus: String, adapter: u16, stop: watch::Receiver<bool>) {
        let manager = self.clone();
        self.run_with(stop, move |operation, config, feed, cancel| {
            let bus = bus.clone();
            let manager = manager.clone();
            Box::pin(async move {
                match operation {
                    Operation::Poll => worker::run_on(config, feed, cancel, &bus, adapter)
                        .await
                        .map(|_| Outcome::Done),
                    Operation::Pair => {
                        worker::enroll_with_progress(config, cancel, &bus, adapter, |_| {})
                            .await
                            .map(|_| Outcome::Done)
                    }
                    Operation::ClimatePair => worker::enroll_for(
                        config,
                        cancel,
                        &bus,
                        adapter,
                        EnrollmentPermission::Driver,
                        |_| {},
                    )
                    .await
                    .map(|_| Outcome::Done),
                    Operation::ClimateQuery | Operation::ClimateCommand(_) => {
                        let command = match operation {
                            Operation::ClimateCommand(c) => Some(c),
                            _ => None,
                        };
                        super::climate::run(config, cancel, &bus, adapter, command, || {
                            manager.state.lock().unwrap().climate.authorized = true;
                        })
                        .await
                        .map(Outcome::Climate)
                    }
                }
            })
        })
        .await;
    }
    async fn run_with(
        self: Arc<Self>,
        mut stop: watch::Receiver<bool>,
        runner: impl Fn(Operation, Config, Arc<Feed>, watch::Receiver<bool>) -> Job,
    ) {
        let Some(mut requests) = self.receiver.lock().unwrap().take() else {
            return;
        };
        let mut retries = self.feed.retries();
        let Some(mut climate_requests) = self.feed.take_climate_requests() else { return; };
        let mut native: Option<f50_server::vehicle::ClimateRequest> = None;
        let mut active = None;
        if !*stop.borrow() {
            self.start(Operation::Poll, &mut active, &runner);
        }
        loop {
            self.publish_climate();
            if *stop.borrow() {
                break;
            }
            let deadline = active.as_ref().and_then(|a| a.deadline);
            tokio::select! { biased;
                _ = stop.changed() => break,
                request = requests.recv() => {
                    let Some(request) = request else { break };
                    if request.reply.is_closed() { continue; }
                    let interrupts = matches!(request.command, Command::Configure { .. } | Command::Connect | Command::Disconnect | Command::Pair | Command::Cancel);
                    let result = self.apply(request.command, &mut active, &runner).await;
                    if result.is_ok() && interrupts { native.take(); }
                    let _ = request.reply.send(result.map(|_| self.status()));
                }
                request = climate_requests.recv() => {
                    let Some(request) = request else { break; };
                    let command = request.command.map_or(Command::ClimateQuery, Command::ClimateCommand);
                    if !request.valid(self.feed.climate().context)
                        || active.as_ref().is_some_and(|a| a.operation != Operation::Poll)
                        || self.validate_command(&command).is_err()
                        || self.state.lock().unwrap().disconnected { let _ = request.reply.send(false); continue; }
                    if self.stop_active(&mut active).await.is_err() { let _ = request.reply.send(false); continue; }
                    if !request.valid(self.feed.climate().context) || self.validate_command(&command).is_err() {
                        let _ = request.reply.send(false); self.resume(&mut active, &runner); continue;
                    }
                    let operation = request.command.map_or(Operation::ClimateQuery, Operation::ClimateCommand);
                    self.start(operation, &mut active, &runner);
                    native = Some(request);
                }
                _ = async { match native.as_mut() {
                    Some(request) => { tokio::select! { _ = request.reply.closed() => {}, _ = request.grant.changed() => {} } },
                    None => std::future::pending().await,
                }} => {
                    native.take();
                    let command_pending = active.as_ref().is_some_and(|a| matches!(a.operation, Operation::ClimateCommand(_)));
                    if self.stop_active(&mut active).await.is_ok() {
                        let mut s = self.state.lock().unwrap();
                        if command_pending { s.climate.last_command_status = "unconfirmed"; }
                        s.climate.phase = "cancelled";
                        s.climate.clear_reading();
                        drop(s); self.resume(&mut active, &runner);
                    }
                }
                _ = retries.changed() => {
                    let allowed = { let s = self.state.lock().unwrap(); !s.disconnected && !s.cleanup_failed && active.as_ref().is_none_or(|a| a.operation == Operation::Poll) };
                    if allowed && self.stop_active(&mut active).await.is_ok() {
                        self.reload();
                        self.start(Operation::Poll, &mut active, &runner);
                    }
                }
                _ = async { match deadline { Some(d) => tokio::time::sleep_until(d).await, None => std::future::pending().await } } => {
                    let operation = active.as_ref().unwrap().operation;
                    if self.stop_active(&mut active).await.is_ok() {
                        if matches!(operation, Operation::Pair) { self.phase("pairing_timeout", "failed"); }
                        else { self.climate_finished(operation, false, None); self.resume(&mut active, &runner); }
                    }
                    if let Some(request) = native.take() { let _ = request.reply.send(false); }
                }
                result = async { match active.as_mut() { Some(a) => (&mut a.task).await, None => std::future::pending().await } } => {
                    let operation = active.take().unwrap().operation;
                    let confirmed = result.is_ok();
                    self.completed(operation, result);
                    if let Some(request) = native.take() {
                        let valid = request.valid(self.feed.climate().context);
                        if request.command.is_some() { self.state.lock().unwrap().climate.clear_reading(); }
                        self.publish_climate();
                        let _ = request.reply.send(confirmed && valid && !self.state.lock().unwrap().cleanup_failed);
                    }
                    if matches!(operation, Operation::ClimatePair | Operation::ClimateQuery | Operation::ClimateCommand(_)) { self.resume(&mut active, &runner); }
                }
            }
        }
        native.take();
        let _ = self.stop_active(&mut active).await;
        self.feed.reset_climate();
        if !self.state.lock().unwrap().cleanup_failed {
            self.phase("stopped", "idle");
        }
    }
    fn publish_climate(&self) {
        let s = self.state.lock().unwrap();
        let allowed = s.saved.as_ref().is_some_and(|c| c.enabled) && !s.disconnected && !s.cleanup_failed && s.climate.authorized;
        self.feed.publish_climate(allowed, s.climate.state.clone(), s.climate.received_ms);
    }
    fn reload(&self) {
        match load(&self.directory.join("config.json"), &self.legacy) {
            Ok(saved) => {
                let fingerprint = saved
                    .as_ref()
                    .and_then(|s| fingerprint(&s.private_key).ok());
                let climate = climate_status(saved.as_ref());
                let mut s = self.state.lock().unwrap();
                let changed = match (&s.saved, &saved) {
                    (Some(old), Some(new)) => {
                        old.vin != new.vin
                            || old.private_key != new.private_key
                            || old.control_private_key != new.control_private_key
                    }
                    _ => true,
                };
                if changed
                    || s.climate.public_key_fingerprint != climate.public_key_fingerprint
                    || !saved.as_ref().is_some_and(|c| c.enabled)
                {
                    s.climate = climate;
                    self.feed.reset_climate();
                }
                s.saved = saved;
                s.fingerprint = fingerprint;
                s.phase = None;
            }
            Err(_) => {
                self.feed.reset_climate();
                {
                    let mut s = self.state.lock().unwrap();
                    s.saved = None;
                    s.fingerprint = None;
                    s.climate = ClimateStatus::default();
                }
                self.feed.reset();
                self.phase("configuration_error", "idle");
            }
        }
    }
    fn start(
        &self,
        operation: Operation,
        active: &mut Option<Active>,
        runner: &impl Fn(Operation, Config, Arc<Feed>, watch::Receiver<bool>) -> Job,
    ) {
        let mut s = self.state.lock().unwrap();
        if s.cleanup_failed {
            return;
        }
        let Some(saved) = s.saved.clone() else {
            s.phase = Some(if s.phase == Some("configuration_error") {
                "configuration_error"
            } else {
                "unconfigured"
            });
            self.feed.status(false, false, s.phase.unwrap());
            return;
        };
        if !saved.enabled {
            s.phase = Some("disabled");
            self.feed.status(false, false, "disabled");
            return;
        }
        let climate = matches!(
            operation,
            Operation::ClimatePair | Operation::ClimateQuery | Operation::ClimateCommand(_)
        );
        if if climate {
            !s.climate.key_ready
        } else {
            s.fingerprint.is_none()
        } {
            s.phase = Some("configuration_error");
            self.feed.status(true, false, "configuration_error");
            return;
        }
        self.feed.reset();
        let pairing = matches!(operation, Operation::Pair | Operation::ClimatePair);
        s.phase = pairing.then_some("pairing");
        s.deadline = pairing.then(|| now_ms().saturating_add(PAIR_WINDOW.as_millis() as u64));
        if pairing {
            s.pairing = "pairing";
        }
        if climate {
            s.climate.phase = match operation {
                Operation::ClimatePair => "pairing",
                Operation::ClimateQuery => "querying",
                _ => "command_pending",
            };
            s.phase = Some(s.climate.phase);
            s.climate.authorized = false;
            if matches!(operation, Operation::ClimateCommand(_)) {
                s.climate.last_command_status = "pending";
            }
        }
        self.feed
            .status(true, false, if pairing { "pairing" } else { "connecting" });
        let (cancel, receiver) = watch::channel(false);
        let mut config = saved.config();
        if climate {
            config.private_key = saved.control_private_key.unwrap();
        }
        let deadline = if pairing {
            Some(PAIR_WINDOW)
        } else if climate {
            Some(Duration::from_secs(40))
        } else {
            None
        };
        *active = Some(Active {
            operation,
            cancel,
            task: runner(operation, config, self.feed.clone(), receiver),
            deadline: deadline.map(|d| tokio::time::Instant::now() + d),
        });
    }
    async fn stop_active(&self, active: &mut Option<Active>) -> std::result::Result<(), String> {
        if let Some(mut active) = active.take() {
            let _ = active.cancel.send(true);
            let result = (&mut active.task).await;
            if self.uncertain(&result) {
                return self.cleanup_error();
            }
        }
        Ok(())
    }
    fn uncertain(&self, result: &Result<Outcome>) -> bool {
        self.feed.snapshot().phase == "cleanup_failed"
            || result
                .as_ref()
                .err()
                .and_then(|e| e.downcast_ref::<super::transport::Failure>())
                .is_some_and(|e| e.cleanup_uncertain)
    }
    fn cleanup_error(&self) -> std::result::Result<(), String> {
        self.feed.reset_climate();
        self.state.lock().unwrap().cleanup_failed = true;
        self.phase("cleanup_failed", "failed");
        Err("Tesla连接清理未确认；已停止后续连接，请检查服务状态".into())
    }
    fn completed(&self, operation: Operation, result: Result<Outcome>) {
        if self.uncertain(&result) {
            let _ = self.cleanup_error();
            return;
        }
        if operation == Operation::Pair {
            if result.is_ok() {
                self.phase("pairing_succeeded", "succeeded");
            } else {
                self.phase("pairing_failed", "failed");
            }
        } else if matches!(
            operation,
            Operation::ClimatePair | Operation::ClimateQuery | Operation::ClimateCommand(_)
        ) {
            let success = result.is_ok();
            let reading = match result {
                Ok(Outcome::Climate(state)) => state,
                _ => None,
            };
            self.climate_finished(operation, success, reading);
        } else if self.feed.snapshot().phase == "connecting" {
            self.phase("configuration_error", "idle");
        }
    }
    fn climate_finished(&self, operation: Operation, success: bool, reading: Option<ClimateState>) {
        let mut s = self.state.lock().unwrap();
        s.deadline = None;
        s.climate.phase = if success { "ready" } else { "failed" };
        if success {
            s.climate.authorized = true;
        }
        match operation {
            Operation::ClimatePair => {
                s.climate.authorized = success;
                s.pairing = if success { "succeeded" } else { "failed" };
            }
            Operation::ClimateCommand(_) => {
                s.climate.last_command_status = if success { "confirmed" } else { "unconfirmed" };
            }
            Operation::ClimateQuery => {
                if let Some(state) = reading {
                    s.climate.state = Some(state);
                    s.climate.received_ms = Some(now_ms());
                } else {
                    s.climate.clear_reading();
                }
            }
            _ => {}
        }
    }
    fn resume(
        &self,
        active: &mut Option<Active>,
        runner: &impl Fn(Operation, Config, Arc<Feed>, watch::Receiver<bool>) -> Job,
    ) {
        let s = self.state.lock().unwrap();
        let resume = !s.disconnected && !s.cleanup_failed;
        drop(s);
        if resume {
            self.start(Operation::Poll, active, runner);
        } else if !self.state.lock().unwrap().cleanup_failed {
            self.phase("disconnected", "idle");
        }
    }
    fn phase(&self, phase: &'static str, pairing: &'static str) {
        let enabled = {
            let mut s = self.state.lock().unwrap();
            s.phase = Some(phase);
            s.pairing = pairing;
            s.deadline = None;
            s.saved.as_ref().is_some_and(|c| c.enabled)
        };
        self.feed.status(enabled, false, phase);
    }
    async fn apply(
        &self,
        command: Command,
        active: &mut Option<Active>,
        runner: &impl Fn(Operation, Config, Arc<Feed>, watch::Receiver<bool>) -> Job,
    ) -> std::result::Result<(), String> {
        if self.state.lock().unwrap().cleanup_failed {
            return Err("Tesla连接清理未确认；请检查服务状态".into());
        }
        let operation = active.as_ref().map(|a| a.operation);
        if matches!(command, Command::Cancel) && !operation.is_some_and(|o| o != Operation::Poll) {
            return Ok(());
        }
        if matches!(
            command,
            Command::ClimatePair | Command::ClimateQuery | Command::ClimateCommand(_)
        ) && operation.is_some_and(|o| o != Operation::Poll)
        {
            return Err("Tesla任务正在进行；请等待完成或先取消".into());
        }
        self.validate_command(&command)?;
        self.stop_active(active).await?;
        if matches!(command, Command::Configure { .. } | Command::Connect | Command::Disconnect | Command::Pair | Command::Cancel | Command::ClimatePair) {
            self.feed.reset_climate();
        }
        if matches!(operation, Some(Operation::ClimateCommand(_))) {
            self.state.lock().unwrap().climate.last_command_status = "unconfirmed";
        }
        match command {
            Command::Configure { vin, enabled } => {
                let vin = vin
                    .or_else(|| {
                        self.state
                            .lock()
                            .unwrap()
                            .saved
                            .as_ref()
                            .map(|s| s.vin.clone())
                    })
                    .ok_or("首次配置Tesla车辆需要完整VIN")?;
                validate_vin(&vin)?;
                private_directory(&self.directory)?;
                let current = self.state.lock().unwrap().saved.clone();
                let previous = current.filter(|s| s.vin == vin).map(|s| s.private_key);
                let key = previous.clone().unwrap_or_else(|| {
                    self.directory
                        .join(format!("{}.pk8", hex(&Sha256::digest(vin.as_bytes()))))
                });
                if previous.is_none() && !key.exists() {
                    worker::keygen(&key)
                        .map_err(|_| "Tesla身份保存失败；已有身份未改动".to_owned())?;
                }
                let fingerprint = fingerprint(&key)
                    .map_err(|_| "Tesla身份无效或权限不安全；已有身份未改动".to_owned())?;
                let control_key = self
                    .state
                    .lock()
                    .unwrap()
                    .saved
                    .as_ref()
                    .filter(|s| s.vin == vin)
                    .and_then(|s| s.control_private_key.clone())
                    .or_else(|| {
                        let path = self.directory.join(format!(
                            "{}.driver.pk8",
                            hex(&Sha256::digest(vin.as_bytes()))
                        ));
                        path.exists().then_some(path)
                    });
                let saved = Saved {
                    vin,
                    private_key: key,
                    enabled,
                    control_private_key: control_key,
                };
                let bytes =
                    serde_json::to_vec(&saved).map_err(|_| "Tesla配置编码失败".to_owned())?;
                f50_server::settings::write_private(&self.directory.join("config.json"), &bytes)
                    .map_err(|_| "Tesla配置保存失败".to_owned())?;
                {
                    let mut s = self.state.lock().unwrap();
                    s.climate = climate_status(Some(&saved));
                    s.saved = Some(saved);
                    s.fingerprint = Some(fingerprint);
                    s.disconnected = false;
                    s.phase = None;
                    s.pairing = "idle";
                    s.deadline = None;
                }
                self.feed.reset();
                self.start(Operation::Poll, active, runner);
            }
            Command::Connect => {
                self.reload();
                {
                    let mut s = self.state.lock().unwrap();
                    if !s.saved.as_ref().is_some_and(|c| c.enabled) {
                        return Err("请先配置并启用Tesla车辆".into());
                    }
                    s.disconnected = false;
                }
                self.start(Operation::Poll, active, runner);
            }
            Command::Disconnect => {
                let mut s = self.state.lock().unwrap();
                s.disconnected = true;
                s.climate.clear_reading();
                s.climate.phase = "disconnected";
                if matches!(operation, Some(Operation::ClimateCommand(_))) {
                    s.climate.last_command_status = "unconfirmed";
                }
                drop(s);
                self.phase("disconnected", "idle");
            }
            Command::Pair => {
                if !self
                    .state
                    .lock()
                    .unwrap()
                    .saved
                    .as_ref()
                    .is_some_and(|c| c.enabled)
                {
                    return Err("请先配置并启用Tesla车辆".into());
                }
                self.state.lock().unwrap().disconnected = false;
                self.start(Operation::Pair, active, runner);
            }
            Command::Cancel => {
                if operation == Some(Operation::Pair) {
                    self.state.lock().unwrap().disconnected = true;
                    self.phase("pairing_cancelled", "cancelled");
                } else {
                    {
                        let mut s = self.state.lock().unwrap();
                        s.climate.phase = "cancelled";
                        s.deadline = None;
                        if matches!(operation, Some(Operation::ClimatePair)) {
                            s.pairing = "cancelled";
                        }
                        if matches!(operation, Some(Operation::ClimateCommand(_))) {
                            s.climate.last_command_status = "unconfirmed";
                        }
                    }
                    self.resume(active, runner);
                }
            }
            Command::ClimatePair => {
                private_directory(&self.directory)?;
                let mut saved = self
                    .state
                    .lock()
                    .unwrap()
                    .saved
                    .clone()
                    .ok_or("请先配置Tesla车辆")?;
                let key = saved.control_private_key.clone().unwrap_or_else(|| {
                    self.directory.join(format!(
                        "{}.driver.pk8",
                        hex(&Sha256::digest(saved.vin.as_bytes()))
                    ))
                });
                if saved.control_private_key.is_none() && !key.exists() {
                    worker::keygen(&key).map_err(|_| "Tesla空调身份保存失败")?;
                }
                fingerprint(&key).map_err(|_| "Tesla空调身份无效或权限不安全")?;
                saved.control_private_key = Some(key);
                f50_server::settings::write_private(
                    &self.directory.join("config.json"),
                    &serde_json::to_vec(&saved).map_err(|_| "Tesla配置编码失败")?,
                )
                .map_err(|_| "Tesla空调配置保存失败")?;
                {
                    let mut s = self.state.lock().unwrap();
                    s.climate = climate_status(Some(&saved));
                    s.saved = Some(saved);
                }
                self.start(Operation::ClimatePair, active, runner);
            }
            Command::ClimateQuery => self.start(Operation::ClimateQuery, active, runner),
            Command::ClimateCommand(command) => {
                self.start(Operation::ClimateCommand(command), active, runner)
            }
        }
        Ok(())
    }
    fn validate_command(&self, command: &Command) -> std::result::Result<(), String> {
        if !matches!(
            command,
            Command::ClimatePair | Command::ClimateQuery | Command::ClimateCommand(_)
        ) {
            return Ok(());
        }
        let s = self.state.lock().unwrap();
        if !s.saved.as_ref().is_some_and(|c| c.enabled) {
            return Err("请先配置并启用Tesla车辆".into());
        }
        if !matches!(command, Command::ClimatePair) && !s.climate.key_ready {
            return Err("请先显式申请空调所需的Driver权限".into());
        }
        if let Command::ClimateCommand(command) = command {
            if !s.climate.authorized {
                return Err("请先刷新空调数据验证Driver授权".into());
            }
            if let ClimateCommand::SetTemperature {
                driver_celsius,
                passenger_celsius,
            } = command
            {
                let bounds = s
                    .climate
                    .state
                    .as_ref()
                    .and_then(|c| c.min_temp_celsius.zip(c.max_temp_celsius));
                if !s.climate.fresh(now_ms()) {
                    return Err("空调数据未确认新鲜；请先刷新车辆数据".into());
                }
                let (min, max) = bounds
                    .filter(|(min, max)| min.is_finite() && max.is_finite() && min <= max)
                    .ok_or("车辆温度范围未知；请先刷新空调数据")?;
                if [*driver_celsius, *passenger_celsius]
                    .iter()
                    .any(|t| !t.is_finite() || !(0.0..=50.0).contains(t) || *t < min || *t > max)
                {
                    return Err("温度超出车辆当前支持的范围".into());
                }
            }
        }
        Ok(())
    }
}
fn climate_status(saved: Option<&Saved>) -> ClimateStatus {
    let fingerprint = saved
        .and_then(|s| s.control_private_key.as_ref())
        .and_then(|p| fingerprint(p).ok());
    ClimateStatus {
        key_ready: fingerprint.is_some(),
        public_key_fingerprint: fingerprint.clone(),
        phase: if fingerprint.is_some() {
            "authorization_unknown"
        } else {
            "unconfigured"
        },
        ..Default::default()
    }
}
fn validate_vin(vin: &str) -> std::result::Result<(), String> {
    if vin.len() != 17
        || !vin
            .bytes()
            .all(|c| c.is_ascii_digit() || (c.is_ascii_uppercase() && !b"IOQ".contains(&c)))
    {
        return Err("VIN须为17位字母或数字，不含I、O、Q".into());
    }
    Ok(())
}
pub fn load_monitor_config(path: &Path) -> Result<Config> {
    let bytes = f50_server::settings::read(
        path.to_str().ok_or("invalid Tesla configuration path")?,
        4096,
    )?;
    Ok(monitor_configuration(&bytes)?.0)
}
pub(super) fn monitor_configuration(bytes: &[u8]) -> Result<(Config, bool)> {
    let saved: Saved = serde_json::from_slice(bytes)?;
    validate_vin(&saved.vin).map_err(|_| "invalid Tesla VIN")?;
    Ok((saved.config(), saved.enabled))
}
fn load(managed: &Path, legacy: &Path) -> Result<Option<Saved>> {
    let path = if managed.exists() { managed } else { legacy };
    let bytes = match f50_server::settings::read(
        path.to_str().ok_or("invalid configuration path")?,
        4096,
    ) {
        Ok(b) => b,
        Err(_) if !path.exists() => return Ok(None),
        Err(_) => return Err("Tesla configuration unreadable".into()),
    };
    let saved: Saved = serde_json::from_slice(&bytes)?;
    validate_vin(&saved.vin).map_err(|_| "invalid Tesla VIN")?;
    Ok(Some(saved))
}
fn private_directory(path: &Path) -> std::result::Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("Tesla私有目录创建失败".into()),
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| "Tesla私有目录不可读取")?;
    if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        return Err("Tesla目录必须为服务用户所有、仅所有者可访问的真实目录".into());
    }
    Ok(())
}
fn fingerprint(path: &Path) -> Result<String> {
    use std::{
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        return Err("unsafe Tesla identity permissions".into());
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err("Tesla identity too large".into());
    }
    Ok(hex(&Sha256::digest(
        Identity::from_pkcs8(&bytes)?.public_key(),
    )))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests;
