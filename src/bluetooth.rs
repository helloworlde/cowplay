//! BlueZ callbacks are authenticated to the daemon's unique bus owner.
use crate::{PairingWindow, Phase};
mod controller_name;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch};
use zbus::{
    Connection, Proxy,
    message::Header,
    zvariant::{ObjectPath, OwnedFd, OwnedObjectPath, OwnedValue, Value},
};
pub const UUID: &str = "00000000-deca-fade-deca-deafdecacafe";
pub const BLUETOOTH_NAME: &str = "cowplay";
const AGENT: &str = "/dev/carplay/agent";
const PROFILE: &str = "/dev/carplay/profile";
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum BtError {
    Rejected(String),
    Canceled(String),
}
fn rejected() -> BtError {
    BtError::Rejected("pairing or connection not authorized".into())
}
struct Attempt {
    window: PairingWindow,
    generation: u64,
    device: Option<String>,
    reply: Option<oneshot::Sender<bool>>,
}
struct Active {
    epoch: u64,
    stop: watch::Sender<bool>,
    done: watch::Sender<bool>,
}
struct Discovery {
    stop: watch::Sender<bool>,
}
// Declared after the operation gate: on future cancellation this is dropped before unlocking it.
struct PendingProfile<'a>(&'a Mutex<State>);
impl Drop for PendingProfile<'_> {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();
        if state.profile_request == ProfileRequest::Opening && state.active.is_none() {
            state.profile_request = ProfileRequest::Cancelling;
        }
    }
}
pub struct Channel {
    pub fd: std::os::fd::OwnedFd,
    pub epoch: u64,
    pub selection: u64,
    pub device: String,
    pub stop: watch::Receiver<bool>,
}
#[derive(Default, PartialEq)]
enum ProfileRequest {
    #[default]
    Idle,
    Opening,
    Cancelling,
}
#[derive(Default)]
struct State {
    stopping: bool,
    selection: u64,
    profile_target: Option<String>,
    profile_request: ProfileRequest,
    attempt: Option<Attempt>,
    generation: u64,
    phone: Option<String>,
    active: Option<Active>,
    epoch: u64,
    discovery: Option<Discovery>,
    discovered: HashSet<String>,
    pairing_stop: Option<watch::Sender<bool>>,
    pairing_error: Option<String>,
    /// A phone whose pairing just completed; CarPlay is invited for it before any default phone.
    fresh: Option<String>,
    /// The fresh phone's invitation has been handed to a task (outgoing Pair starts its own).
    fresh_started: bool,
    adapter_name: Option<String>,
}
impl State {
    fn pairing_open(&mut self) -> bool {
        self.attempt.as_mut().is_some_and(|a| a.window.is_open(Instant::now()))
    }
    fn attempt_phase(&mut self) -> Option<Phase> {
        self.attempt.as_mut().map(|a| a.window.phase(Instant::now()))
    }
}
#[derive(serde::Serialize)]
pub struct Status {
    pub phase: &'static str,
    pub generation: u64,
    pub request: Option<u64>,
    pub passkey: Option<String>,
    /// Seconds left in an open pairing window.
    pub remaining_s: Option<u64>,
    pub rfcomm: bool,
    pub paired: bool,
    pub carplay: &'static str,
    pub selected: Option<String>,
    pub adapter_name: Option<String>,
    pub discovering: bool,
    pub pairing_device: Option<String>,
    pub pairing_error: Option<String>,
}
pub struct Manager {
    pub connection: Connection,
    private_runtime: bool,
    adapter_index: u16,
    adapter_path: String,
    controller: controller_name::Controller,
    advertised_name: String,
    owner: String,
    state: Mutex<State>,
    gate: tokio::sync::Mutex<()>,
    phone_file: PathBuf,
    channels: mpsc::Sender<Channel>,
}
fn phone_path(path: &str, adapter: u16) -> bool { path.starts_with(&format!("/org/bluez/hci{adapter}/dev_")) && f50_server::phones::device_component(path).is_ok() }
#[derive(Clone, serde::Serialize)]
pub struct SavedPhone { pub id: String, pub name: String, pub selected: bool, pub connected: bool }
#[derive(serde::Serialize)]
pub struct NearbyDevice {
    pub id: String,
    pub name: String,
    pub iphone: bool,
    pub phone: bool,
    pub rssi: Option<i16>,
}
#[derive(serde::Serialize)]
pub struct AdapterInfo { pub alias:String, pub name:String, pub controller_name:String, pub eir_name:Option<String>, pub powered:bool, pub discoverable:bool, pub pairable:bool }
type Properties = HashMap<String, OwnedValue>;
type Interfaces = HashMap<String, Properties>;

impl Manager {
    pub(crate) fn private_runtime(&self) -> bool { self.private_runtime }
    pub async fn register(
        bus: &str,
        phone_file: &Path,
    ) -> crate::RuntimeResult<(Arc<Self>, mpsc::Receiver<Channel>)> {
        Self::register_on(bus, phone_file, 0, true).await
    }
    pub async fn register_on(bus: &str, phone_file: &Path, adapter_index: u16, default_agent: bool) -> crate::RuntimeResult<(Arc<Self>, mpsc::Receiver<Channel>)> {
        Self::register_named_on(bus,phone_file,adapter_index,default_agent,BLUETOOTH_NAME).await
    }
    pub async fn register_named_on(bus: &str, phone_file: &Path, adapter_index: u16, default_agent: bool, name: &str) -> crate::RuntimeResult<(Arc<Self>, mpsc::Receiver<Channel>)> {
        if name.is_empty() || name.len()>63 || name.chars().any(char::is_control) { return Err("接收端名称无效".into()); }
        let adapter_path = format!("/org/bluez/hci{adapter_index}");
        let connection = zbus::connection::Builder::address(bus)?
            .method_timeout(Duration::from_secs(5))
            .build()
            .await?;
        let owner = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let dbus = Proxy::new(
                    &connection,
                    "org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                )
                .await?;
                if let Ok(owner) = dbus
                    .call::<_, _, String>("GetNameOwner", &("org.bluez",))
                    .await
                {
                    let adapter = Proxy::new(
                        &connection,
                        owner.as_str(),
                        adapter_path.as_str(),
                        "org.bluez.Adapter1",
                    )
                    .await?;
                    if adapter.get_property::<bool>("Powered").await.is_ok() {
                        return Ok::<_, zbus::Error>(owner);
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await??;
        let (channels, rx) = mpsc::channel(1);
        let manager = Arc::new(Self {
            connection: connection.clone(),
            private_runtime: bus == crate::platform::f50::BUS,
            controller: controller_name::Controller::new(adapter_index),
            advertised_name: name.to_owned(),
            adapter_index,
            adapter_path,
            owner,
            state: Default::default(),
            gate: Default::default(),
            phone_file: phone_file.into(),
            channels,
        });
        let registration = async {
            connection
                .object_server()
                .at(AGENT, Agent(Arc::downgrade(&manager)))
                .await?;
            connection
                .object_server()
                .at(PROFILE, Profile(Arc::downgrade(&manager)))
                .await?;
            let agents = manager
                .proxy("/org/bluez", "org.bluez.AgentManager1")
                .await?;
            agents
                .call::<_, _, ()>(
                    "RegisterAgent",
                    &(ObjectPath::try_from(AGENT)?, "DisplayYesNo"),
                )
                .await?;
            if default_agent {
                agents.call::<_, _, ()>("RequestDefaultAgent", &(ObjectPath::try_from(AGENT)?,)).await?;
            }
            let profiles = manager
                .proxy("/org/bluez", "org.bluez.ProfileManager1")
                .await?;
            let options: HashMap<&str, Value<'_>> = HashMap::from([
                ("Name", Value::from("Rust iAP2 client")),
                ("Role", Value::from("client")),
                ("RequireAuthentication", Value::from(true)),
                ("AutoConnect", Value::from(false)),
            ]);
            if let Err(error) = profiles
                .call::<_, _, ()>(
                    "RegisterProfile",
                    &(ObjectPath::try_from(PROFILE)?, UUID, options),
                )
                .await {
                if matches!(&error, zbus::Error::MethodError(_, detail, _) if detail.as_deref().is_some_and(|detail| detail.contains("UUID already registered"))) {
                    return Err("同一 BlueZ 服务已有接收程序注册 iAP2；更换蓝牙适配器不能解决，请在主机协调接收服务后重启 CowPlay".into());
                }
                return Err(error.into());
            }
            manager.set("Powered", true).await?;
            manager.set_name().await?;
            manager.set_visible(false).await?;
            if let Ok(phone) = std::fs::read_to_string(phone_file) {
                let phone = phone.trim();
                if !phone_path(phone, manager.adapter_index) {
                    return Err("invalid stored Bluetooth device path".into());
                }
                if manager
                    .proxy(phone, "org.bluez.Device1")
                    .await?
                    .get_property::<bool>("Paired")
                    .await
                    .unwrap_or(false)
                {
                    manager.state.lock().unwrap().phone = Some(phone.into());
                }
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        }
        .await;
        if let Err(e) = registration {
            manager.stop().await;
            return Err(e);
        }
        Ok((manager, rx))
    }
    async fn proxy<'a>(&'a self, path: &'a str, interface: &'a str) -> zbus::Result<Proxy<'a>> {
        Proxy::new(&self.connection, self.owner.as_str(), path, interface).await
    }
    fn sender(&self, header: &Header<'_>) -> std::result::Result<(), BtError> {
        if header.sender().is_some_and(|s| s.as_str() == self.owner) {
            Ok(())
        } else {
            Err(rejected())
        }
    }
    pub fn adapter_index(&self) -> u16 { self.adapter_index }
    pub fn advertised_name(&self) -> &str { &self.advertised_name }
    async fn adapter(&self) -> zbus::Result<Proxy<'_>> {
        self.proxy(&self.adapter_path, "org.bluez.Adapter1").await
    }
    async fn set(&self, property: &str, value: bool) -> crate::RuntimeResult<()> {
        self.adapter().await?.set_property(property, value).await?;
        Ok(())
    }
    async fn set_name(&self) -> crate::RuntimeResult<()> {
        self.state.lock().unwrap().adapter_name = None;
        // Do not expose a stale controller name while reconciling the software and hardware.
        self.set_visible(false).await?;
        let adapter = self.adapter().await?;
        let actual: String = adapter.get_property("Alias").await?;
        // BlueZ 5.83 ignores an Alias equal to its stored value even if current_alias differs.
        if actual != self.advertised_name { adapter.set_property("Alias", "").await?; }
        adapter.set_property("Alias", self.advertised_name.as_str()).await?;
        let name = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let name: String = adapter.get_property("Alias").await?;
                if name == self.advertised_name { return Ok::<_,zbus::Error>(name); }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }).await.map_err(|_| "蓝牙名称未生效，请展开调试信息核对适配器名称")??;
        let actual = self.controller.synchronize(&name).await?;
        self.state.lock().unwrap().adapter_name = Some(actual.controller_name);
        Ok(())
    }
    pub async fn adapter_info(&self) -> crate::RuntimeResult<AdapterInfo> {
        tokio::time::timeout(Duration::from_secs(2), async {
            let p:Properties = self.proxy(&self.adapter_path, "org.freedesktop.DBus.Properties").await?
                .call("GetAll", &("org.bluez.Adapter1",)).await?;
            let string=|k:&str| p.get(k).and_then(|v|<&str>::try_from(v).ok()).map(str::to_owned).ok_or("蓝牙名称未知");
            let boolean=|k:&str| p.get(k).and_then(|v|bool::try_from(v).ok()).ok_or("蓝牙适配器状态未知");
            let actual=self.controller.inspect().await?;
            Ok::<_,Box<dyn std::error::Error+Send+Sync>>(AdapterInfo {alias:string("Alias")?,name:string("Name")?,controller_name:actual.controller_name,eir_name:actual.eir_name,powered:boolean("Powered")?,discoverable:boolean("Discoverable")?,pairable:boolean("Pairable")?})
        }).await.map_err(|_| "读取蓝牙适配器信息超时")?
    }
    /// Pairable and discoverable together: only an open pairing window is visible to iPhones.
    async fn set_visible(&self, visible: bool) -> crate::RuntimeResult<()> {
        self.set("Pairable", visible).await?;
        self.set("Discoverable", visible).await
    }
    pub async fn open(self: &Arc<Self>) -> crate::RuntimeResult<()> {
        let _guard = self.gate.lock().await;
        {
            let mut state = self.state.lock().unwrap();
            if state.stopping {
                return Err("service is stopping".into());
            }
            if state.active.is_some()
                || state.pairing_stop.is_some()
                || state.profile_request != ProfileRequest::Idle
                || state.pairing_open()
            {
                return Err("pairing already active".into());
            }
            state.generation = state
                .generation
                .checked_add(1)
                .ok_or("generation exhausted")?;
            state.attempt = Some(Attempt {
                window: PairingWindow::new(Instant::now()),
                generation: state.generation,
                device: None,
                reply: None,
            });
            state.pairing_error = None;
            state.discovered.clear();
            state.fresh = None;
            state.fresh_started = false;
        }
        let result = async {
            self.set_name().await?;
            self.set_visible(true).await?;
            self.start_discovery_locked().await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        }
        .await;
        if result.is_err() {
            let _ = self.cancel_locked().await;
        }
        result
    }
    async fn start_discovery_locked(self: &Arc<Self>) -> crate::RuntimeResult<()> {
        use zbus::export::futures_core::Stream;
        // Subscribe before StartDiscovery: only observations from this window enter the nearby list.
        let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal)
            .sender(self.owner.as_str())?.build();
        let mut signals = zbus::MessageStream::for_match_rule(rule, &self.connection, Some(256)).await?;
        let adapter = self.adapter().await?;
        let filter = HashMap::from([("Transport", Value::from("bredr"))]);
        adapter.call::<_, _, ()>("SetDiscoveryFilter", &(filter,)).await?;
        let (stop, mut stopped) = watch::channel(false);
        let generation = {
            let mut state = self.state.lock().unwrap();
            state.discovery = Some(Discovery { stop });
            state.generation
        };
        adapter.call::<_, _, ()>("StartDiscovery", &()).await?;
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let message = tokio::select! {biased;
                    _ = stopped.changed() => break,
                    message = std::future::poll_fn(|cx| std::pin::Pin::new(&mut signals).poll_next(cx)) => message,
                };
                let Some(Ok(message)) = message else { break };
                let Some(manager) = weak.upgrade() else { break };
                let header = message.header();
                let path = match (header.interface().map(|s| s.as_str()), header.member().map(|s| s.as_str())) {
                    (Some("org.freedesktop.DBus.ObjectManager"), Some("InterfacesAdded")) => {
                        message.body().deserialize::<(OwnedObjectPath, Interfaces)>().ok()
                            .filter(|(_, interfaces)| interfaces.contains_key("org.bluez.Device1"))
                            .map(|(path, _)| path.to_string())
                    }
                    (Some("org.freedesktop.DBus.Properties"), Some("PropertiesChanged")) => {
                        message.body().deserialize::<(String, Properties, Vec<String>)>().ok()
                            .filter(|(interface, properties, _)| interface == "org.bluez.Device1"
                                && properties.keys().any(|key| matches!(key.as_str(), "RSSI" | "Name" | "Class")))
                            .and_then(|_| header.path().map(|path| path.to_string()))
                    }
                    _ => None,
                };
                if let Some(path) = path.filter(|p| phone_path(p, manager.adapter_index)) {
                    let mut state = manager.state.lock().unwrap();
                    if !state.stopping && state.generation == generation && state.discovery.is_some() {
                        state.discovered.insert(path);
                    }
                }
            }
        });
        Ok(())
    }
    async fn stop_discovery_locked(&self) -> crate::RuntimeResult<()> {
        if self.state.lock().unwrap().discovery.is_none() { return Ok(()) }
        match self.adapter().await?
            .call::<_, _, ()>("StopDiscovery", &()).await {
            Ok(()) => {},
            Err(zbus::Error::MethodError(name,_,_)) if matches!(name.as_str(),"org.bluez.Error.NotAuthorized"|"org.bluez.Error.NotReady") => {},
            Err(error) => return Err(error.into()),
        }
        if let Some(discovery) = self.state.lock().unwrap().discovery.take() {
            let _ = discovery.stop.send(true);
        }
        Ok(())
    }
    pub async fn cancel(&self) -> crate::RuntimeResult<()> {
        let _guard = self.gate.lock().await;
        if self.state.lock().unwrap().stopping {
            return Err("service is stopping".into());
        }
        self.cancel_locked().await
    }
    async fn cancel_locked(&self) -> crate::RuntimeResult<()> {
        let target = {
            let mut state = self.state.lock().unwrap();
            let outgoing = state.pairing_stop.is_some();
            state.attempt.as_mut().and_then(|a| {
                let phase = a.window.phase(Instant::now());
                a.window.cancel();
                a.reply.take();
                if !outgoing && matches!(phase, Phase::Confirm { .. } | Phase::Pairing) {
                    a.device.clone()
                } else {
                    None
                }
            })
        };
        let pairing = self.cancel_outgoing_locked().await;
        let discovery = self.stop_discovery_locked().await;
        if let Some(target) = target {
            self.disconnect_device(&target).await?;
        }
        let a = self.set("Pairable", false).await;
        let b = self.set("Discoverable", false).await;
        pairing?;
        discovery?;
        a?;
        b
    }
    async fn cancel_outgoing_locked(&self) -> crate::RuntimeResult<()> {
        let target = {
            let state = self.state.lock().unwrap();
            state.pairing_stop.as_ref().and_then(|stop| {
                let _ = stop.send(true);
                state.attempt.as_ref().and_then(|attempt| attempt.device.clone())
            })
        };
        if let Some(target) = target {
            // BlueZ 5.83 CancelPairing may UNPAIR a bond completed while SDP is pending.
            // Disconnect cancels bonding/browse without deleting the controller's key.
            self.disconnect_device(&target).await?;
        }
        self.state.lock().unwrap().pairing_stop.take();
        Ok(())
    }
    pub fn status(&self) -> Status {
        let mut state = self.state.lock().unwrap();
        let (phase, generation, request, passkey) = match state.attempt.as_mut() {
            None => ("closed", state.generation, None, None),
            Some(a) => {
                let phase = a.window.phase(Instant::now());
                let (request, key) = match phase {
                    Phase::Confirm { request, passkey } => (Some(request), Some(format!("{passkey:06}"))),
                    _ => (None, None),
                };
                (phase.name(), a.generation, request, key)
            }
        };
        let remaining_s = state.attempt.as_mut().and_then(|a| {
            let now = Instant::now();
            a.window.is_open(now).then(|| a.window.remaining(now).as_secs())
        });
        Status {
            phase,
            generation,
            request,
            passkey,
            remaining_s,
            rfcomm: state.active.is_some(),
            paired: state.phone.is_some(),
            carplay: "not_verified",
            selected: state.phone.clone(),
            adapter_name: state.adapter_name.clone(),
            discovering: state.discovery.is_some(),
            pairing_device: state.attempt.as_ref().and_then(|a| a.device.clone()),
            pairing_error: state.pairing_error.clone(),
        }
    }
    pub fn confirm(&self, generation: u64, request: u64, accept: bool) -> crate::RuntimeResult<()> {
        let mut state = self.state.lock().unwrap();
        let a = state.attempt.as_mut().ok_or("no pairing window")?;
        if a.generation != generation {
            return Err("stale pairing window".into());
        }
        a.window.confirm(Instant::now(), request, accept)?;
        a.reply
            .take()
            .ok_or("Bluetooth request no longer pending")?
            .send(accept)
            .map_err(|_| "Bluetooth request ended")?;
        Ok(())
    }
    async fn observed_pairing(&self, target: &str) -> crate::RuntimeResult<bool> {
        if self.state.lock().unwrap().pairing_stop.is_some() { return Ok(false) }
        self.observe_bonded_pairing(target).await
    }
    async fn observe_bonded_pairing(&self, target: &str) -> crate::RuntimeResult<bool> {
        let generation = {
            let mut state = self.state.lock().unwrap();
            state.attempt.as_mut().and_then(|a| {
                if a.device.as_deref() == Some(target)
                    && a.window.phase(Instant::now()) == Phase::Pairing
                {
                    Some(a.generation)
                } else {
                    None
                }
            })
        };
        let Some(generation) = generation else {
            return Ok(false);
        };
        if !self
            .proxy(target, "org.bluez.Device1")
            .await?
            .get_property::<bool>("Paired")
            .await?
        {
            return Ok(false);
        }
        if !self.proxy(target,"org.bluez.Device1").await?
            .get_property::<bool>("Bonded").await? {
            return Ok(false);
        }
        let mut s = self.state.lock().unwrap();
        if s.stopping {
            return Ok(false);
        }
        let Some(a) = s.attempt.as_mut() else {
            return Ok(false);
        };
        let now = Instant::now();
        if a.generation != generation || a.device.as_deref() != Some(target)
            || a.window.phase(now) != Phase::Pairing {
            return Ok(false);
        }
        // Publish the saved phone only after the private atomic replacement succeeds.
        // Holding state here also prevents a stale confirmation crossing cancellation/windows.
        f50_server::settings::write_private(&self.phone_file, target.as_bytes())?;
        a.window.bluez_paired(now)?;
        s.phone = Some(target.into());
        // An outgoing Pair invites the phone from its own task; an iPhone-initiated pairing is
        // handed to the service loop. Either way a default connect must not call another phone.
        s.fresh_started = s.pairing_stop.is_some();
        s.fresh = Some(target.into());
        f50_server::diag_log!(Info,"bt","蓝牙绑定已完整保存；即将邀请这部手机连接 CarPlay");
        Ok(true)
    }
    /// The newly paired phone that still needs its first CarPlay invitation (returned once).
    pub fn take_new_pairing(&self) -> Option<String> {
        let mut s = self.state.lock().unwrap();
        if s.stopping || s.fresh_started { return None }
        s.fresh_started = s.fresh.is_some();
        s.fresh.clone()
    }
    /// Close discovery after a bond without dropping the freshly paired ACL.
    pub async fn close_completed_pairing(&self,device:&str,selection:u64)->crate::RuntimeResult<()> {
        let _guard=self.gate.lock().await;
        {
            let mut s=self.state.lock().unwrap();
            if s.stopping || s.selection!=selection || s.active.is_some() || s.profile_request!=ProfileRequest::Idle
                || !s.attempt.as_mut().is_some_and(|a|a.device.as_deref()==Some(device)&&a.window.phase(Instant::now())==Phase::Paired) {
                return Err("此操作已被新的手机选择替代".into());
            }
        }
        self.stop_discovery_locked().await?;
        self.set_visible(false).await
    }
    /// While set, an implicit "connect the default phone" request must not pre-empt this phone.
    pub fn fresh_pairing(&self) -> Option<String> { self.state.lock().unwrap().fresh.clone() }
    #[cfg(test)]
    pub(crate) fn set_fresh_for_test(&self, device: &str) { let mut s = self.state.lock().unwrap(); s.fresh = Some(device.into()); s.fresh_started = true; }
    /// The fresh phone's own invitation finished (connected, failed or superseded).
    pub fn finish_new_pairing(&self, device: &str) {
        let mut s = self.state.lock().unwrap();
        if s.fresh.as_deref() == Some(device) { s.fresh = None; s.fresh_started = false; }
    }
    /// A newer user choice (cancel, disconnect, another phone) consumes the hand-off; `keep` survives.
    pub fn supersede_new_pairing(&self, keep: Option<&str>) {
        let mut s = self.state.lock().unwrap();
        if s.fresh.is_some() && s.fresh.as_deref() != keep { s.fresh = None; s.fresh_started = false; }
    }
    pub async fn poll(&self) -> crate::RuntimeResult<()> {
        // The service loop must keep consuming channels while a D-Bus connect is in flight.
        let Ok(_guard) = self.gate.try_lock() else {
            return Ok(());
        };
        let dbus = Proxy::new(
            &self.connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await?;
        let owner: String = dbus.call("GetNameOwner", &("org.bluez",)).await?;
        if owner != self.owner {
            return Err("BlueZ owner changed; restart required".into());
        }
        self.poll_pairing_locked().await
    }
    async fn poll_pairing_locked(&self) -> crate::RuntimeResult<()> {
        let target = {
            let mut s = self.state.lock().unwrap();
            s.attempt.as_mut().and_then(|a| {
                if a.window.phase(Instant::now()) == Phase::Pairing {
                    a.device.clone()
                } else {
                    None
                }
            })
        };
        if let Some(target) = target {
            self.observed_pairing(&target).await?;
        }
        let closed = {
            let mut s = self.state.lock().unwrap();
            s.attempt.as_mut().is_none_or(|a| {
                matches!(
                    a.window.phase(Instant::now()),
                    Phase::Expired | Phase::Cancelled | Phase::Paired
                )
            })
        };
        if closed {
            let paired = self.state.lock().unwrap().attempt_phase() == Some(Phase::Paired);
            if !paired { self.cancel_outgoing_locked().await?; }
            self.stop_discovery_locked().await?;
            self.set_visible(false).await?;
        }
        Ok(())
    }
    pub fn selected_phone(&self) -> Option<String> { self.state.lock().unwrap().phone.clone() }
    /// A pairing window is waiting for an iPhone, showing a code or finishing the bond.
    pub fn pairing_open(&self) -> bool { self.state.lock().unwrap().pairing_open() }
    pub async fn device_lists(&self) -> crate::RuntimeResult<(Vec<SavedPhone>, Vec<NearbyDevice>)> {
        let objects: HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>> =
            tokio::time::timeout(Duration::from_secs(2), async {
                self.proxy("/", "org.freedesktop.DBus.ObjectManager").await?
                    .call("GetManagedObjects", &()).await
            }).await??;
        let (selected, discovered) = {
            let state = self.state.lock().unwrap();
            (state.phone.clone(), state.discovered.clone())
        };
        let mut phones=Vec::new();
        let mut nearby=Vec::new();
        for (path, interfaces) in objects {
            if !phone_path(path.as_str(), self.adapter_index) {continue}
            let Some(p)=interfaces.get("org.bluez.Device1") else {continue};
            let paired=p.get("Paired").and_then(|v| bool::try_from(v).ok())==Some(true)
                && p.get("Bonded").and_then(|v| bool::try_from(v).ok())==Some(true);
            let icon=p.get("Icon").and_then(|v| <&str>::try_from(v).ok()).unwrap_or("");
            let class=p.get("Class").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
            let uuids=p.get("UUIDs").and_then(|v| v.try_clone().ok()).and_then(|v| Vec::<String>::try_from(v).ok()).unwrap_or_default();
            let name:String=p.get("Alias").or_else(||p.get("Name")).and_then(|v| <&str>::try_from(v).ok()).unwrap_or("未命名蓝牙设备").chars().take(128).collect();
            let iphone=name.to_ascii_lowercase().contains("iphone") || uuids.iter().any(|s|s.eq_ignore_ascii_case(UUID));
            let phone=iphone || icon=="phone" || class&0x1f00==0x0200;
            if paired && (selected.as_deref()==Some(path.as_str()) || phone) {
                phones.push(SavedPhone { selected:selected.as_deref()==Some(path.as_str()), connected:p.get("Connected").and_then(|v|bool::try_from(v).ok()).unwrap_or(false),id:path.to_string(),name });
            } else if !paired && discovered.contains(path.as_str()) {
                nearby.push(NearbyDevice { id:path.to_string(), name, iphone, phone,
                    rssi:p.get("RSSI").and_then(|v| i16::try_from(v).ok()) });
            }
        }
        phones.sort_by(|a,b|b.selected.cmp(&a.selected).then(a.name.cmp(&b.name)).then(a.id.cmp(&b.id)));
        nearby.sort_by(|a,b|b.iphone.cmp(&a.iphone).then(b.phone.cmp(&a.phone)).then(a.name.cmp(&b.name)).then(a.id.cmp(&b.id)));
        Ok((phones, nearby))
    }
    pub async fn saved_phones(&self) -> crate::RuntimeResult<Vec<SavedPhone>> {
        Ok(self.device_lists().await?.0)
    }
    pub async fn prepare_pair(&self, device:&str, generation:u64) -> crate::RuntimeResult<watch::Receiver<bool>> {
        let _guard = self.gate.lock().await;
        {
            let mut state = self.state.lock().unwrap();
            if state.stopping || state.pairing_stop.is_some() { return Err("已有配对操作，请先完成或取消".into()) }
            let attempt = state.attempt.as_mut().ok_or("请先扫描附近设备")?;
            if attempt.generation != generation || attempt.device.is_some()
                || attempt.window.phase(Instant::now()) != Phase::Waiting {
                return Err("配对窗口已改变，请重新扫描后选择设备".into())
            }
        }
        if !phone_path(device, self.adapter_index) || !self.device_lists().await?.1.iter().any(|d| d.id == device) {
            return Err("设备不在本次扫描结果中，请重新扫描".into())
        }
        self.stop_discovery_locked().await?;
        let mut state = self.state.lock().unwrap();
        if state.stopping { return Err("service is stopping".into()) }
        let attempt = state.attempt.as_mut().ok_or("配对窗口已关闭")?;
        if attempt.generation != generation || attempt.device.is_some()
            || attempt.window.phase(Instant::now()) != Phase::Waiting {
            return Err("配对窗口已改变，请重新扫描后选择设备".into())
        }
        attempt.device = Some(device.into());
        let (stop, stopped) = watch::channel(false);
        state.pairing_stop = Some(stop);
        Ok(stopped)
    }
    pub async fn pair(&self, device:&str, generation:u64, mut stopped:watch::Receiver<bool>) -> crate::RuntimeResult<()> {
        // Pair waits for both confirmations. Unlike ordinary calls it needs its own bounded
        // deadline, and must leave the operation gate free for confirm/cancel/status requests.
        let result = tokio::select! {biased;
            _ = stopped.changed() => return Err("配对已取消".into()),
            result = tokio::time::timeout(Duration::from_secs(120), async {
                self.proxy(device, "org.bluez.Device1").await?
                    .call_with_flags::<_, _, ()>("Pair", Default::default(), &()).await?;
                Ok::<_, zbus::Error>(())
            }) => result.map_err(|_| "iPhone 配对超时，请保持设置 › 蓝牙页面打开后重试".to_owned())
                .and_then(|r| r.map_err(|e| format!("蓝牙配对未完成：{e}"))),
        };
        let _guard = self.gate.lock().await;
        let phase = {
            let mut state = self.state.lock().unwrap();
            let stopping = state.stopping;
            if stopping || !state.attempt.as_mut().is_some_and(|a|
                a.generation == generation && a.device.as_deref() == Some(device)) {
                return Err("配对已被新的操作替代".into())
            }
            state.attempt.as_mut().unwrap().window.phase(Instant::now())
        };
        if matches!(phase,Phase::Cancelled|Phase::Expired) {
            self.cancel_outgoing_locked().await?;
            return Err("配对已取消或过期".into())
        }
        let result = match result {
            Ok(()) => {
                let observed = self.observe_bonded_pairing(device).await;
                match observed {
                    Err(error) => Err(format!("无法保存配对结果：{error}")),
                    Ok(_) if self.state.lock().unwrap().attempt_phase() == Some(Phase::Paired) => Ok(()),
                    Ok(_) => Err("配对尚未经过两端确认，请重新扫描后重试".to_owned()),
                }
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            {
                let mut state = self.state.lock().unwrap();
                if let Some(attempt) = state.attempt.as_mut() { attempt.window.cancel(); attempt.reply.take(); }
                state.pairing_error = Some(error.clone());
            }
            self.cancel_outgoing_locked().await?;
            return Err(error.into())
        }
        self.state.lock().unwrap().pairing_stop.take();
        Ok(())
    }
    pub async fn validate_phone(&self,device:&str)->crate::RuntimeResult<()> {
        if !phone_path(device, self.adapter_index) || !self.saved_phones().await?.iter().any(|p|p.id==device) {return Err("所选手机未配对，请先配对新 iPhone".into())}
        Ok(())
    }
    pub fn select_phone(&self,device:&str,selection:u64)->std::io::Result<()> {
        if !phone_path(device, self.adapter_index) {return Err(std::io::Error::other("无效的手机选择"))}
        let mut s=self.state.lock().unwrap();
        if s.stopping || s.selection!=selection || s.active.is_some() || s.profile_request!=ProfileRequest::Idle {return Err(std::io::Error::other("旧手机尚未断开，请重试"))}
        f50_server::settings::write_private(&self.phone_file,device.as_bytes())?;
        s.phone=Some(device.into());Ok(())
    }
    /// Revoke the old request before awaiting its gate; late FDs remain unauthorized.
    pub async fn quiesce(&self,selection:u64)->crate::RuntimeResult<()> {
        let target={let mut s=self.state.lock().unwrap();s.selection=selection;
            if s.profile_request==ProfileRequest::Opening {s.profile_request=ProfileRequest::Cancelling;}
            if let Some(active)=&s.active {let _=active.stop.send(true);}
            s.profile_target.clone().or_else(||s.phone.clone())};
        let _guard=self.gate.lock().await;
        self.cancel_locked().await?;
        self.state.lock().unwrap().pairing_error=None;
        if let Some(target)=target {self.cancel_profile_request(&target).await?;}
        self.disconnect_phone().await?;
        Ok(())
    }
    pub async fn connect(&self) -> crate::RuntimeResult<()> {
        let _guard = self.gate.lock().await;
        let (target, cleanup) = {
            let mut state = self.state.lock().unwrap();
            if state.stopping {
                return Err("service is stopping".into());
            }
            if state.active.is_some() {
                return Ok(());
            }
            if state.pairing_open() {
                return Err("pairing is still in progress".into());
            }
            let target = state.phone.clone().ok_or("no confirmed paired phone")?;
            (target, state.profile_request == ProfileRequest::Cancelling)
        };
        if cleanup {
            self.cancel_profile_request(&target).await?;
        }
        {
            let mut state = self.state.lock().unwrap();
            if state.stopping {
                return Err("service is stopping".into());
            }
            state.profile_target = Some(target.clone());
            state.profile_request = ProfileRequest::Opening;
        }
        let _pending = PendingProfile(&self.state);
        let result: zbus::Result<()> = async {
            self.proxy(&target, "org.bluez.Device1")
                .await?
                .call::<_, _, ()>("ConnectProfile", &(UUID,))
                .await
        }
        .await;
        {
            let mut state = self.state.lock().unwrap();
            // The FD is stronger evidence than a lost/late method reply. Do not tear down a live channel.
            if state.active.is_some() {
                return Ok(());
            }
            // Revoke permission before awaiting remote cancellation; an RPC timeout is not cancellation.
            state.profile_request = ProfileRequest::Cancelling;
        }
        f50_server::diag_log!(Info,"bt","iAP2 连接请求结束：failed={}；开始清理未建立的通道",result.is_err());
        let cleanup = self.cancel_profile_request(&target).await;
        match (result, cleanup) {
            (Err(error), Ok(())) => Err(error.into()),
            (Ok(()), Ok(())) => Err("BlueZ returned without an RFCOMM channel".into()),
            (Err(error), Err(cleanup)) => Err(format!("{error}；蓝牙断开清理未完成：{cleanup}").into()),
            (Ok(()), Err(cleanup)) => Err(format!("蓝牙通道未建立，且断开清理未完成：{cleanup}").into()),
        }
    }
    async fn cancel_profile_request(&self, target: &str) -> crate::RuntimeResult<()> {
        // DisconnectProfile does not track pending profile connections. Device.Disconnect cancels
        // the pending connect/SDP operation without removing the saved pairing.
        self.disconnect_device(target).await?;
        let mut state = self.state.lock().unwrap();
        state.profile_request = ProfileRequest::Idle;
        state.profile_target = None;
        Ok(())
    }
    async fn disconnect_device(&self,target:&str)->crate::RuntimeResult<()> {
        // BlueZ 5.83 itself waits 2s before disconnecting a live ACL. Give that
        // transaction one bounded deadline, independent of the ordinary RPC timeout.
        let result=tokio::time::timeout(Duration::from_secs(5), async {
            self.proxy(target, "org.bluez.Device1").await?
                .call_with_flags::<_, _, ()>("Disconnect", Default::default(), &()).await
        }).await.map_err(|_| "蓝牙断开清理超时（5 秒）")?;
        match result {
            Ok(Some(()))=>{},
            Ok(None)=>return Err("蓝牙断开没有返回确认".into()),
            Err(zbus::Error::MethodError(name,_,_)) if matches!(name.as_str(),"org.bluez.Error.NotConnected"|"org.freedesktop.DBus.Error.UnknownObject")=>{},
            Err(error)=>return Err(error.into()),
        }
        Ok(())
    }
    pub fn complete(&self, epoch: u64) {
        let mut s = self.state.lock().unwrap();
        if s.active.as_ref().is_some_and(|a| a.epoch == epoch) {
            let active = s.active.take().unwrap();
            s.profile_request = ProfileRequest::Idle;
            s.profile_target = None;
            let _ = active.done.send(true);
        }
    }
    async fn disconnect(&self) -> std::result::Result<(), BtError> {
        let done = {
            let s = self.state.lock().unwrap();
            s.active.as_ref().map(|a| {
                let _ = a.stop.send(true);
                a.done.subscribe()
            })
        };
        if let Some(mut done) = done {
            if !*done.borrow() {
                tokio::time::timeout(Duration::from_secs(2), done.changed())
                    .await
                    .map_err(|_| BtError::Canceled("RFCOMM close timed out".into()))?
                    .map_err(|_| rejected())?;
            }
        }
        Ok(())
    }
    /// Remove a saved iPhone from BlueZ. The phone with the active channel cannot be forgotten.
    pub async fn forget(&self, device: &str) -> crate::RuntimeResult<()> {
        if !phone_path(device, self.adapter_index) {
            return Err("无效的手机".into());
        }
        let _guard = self.gate.lock().await;
        {
            let s = self.state.lock().unwrap();
            if s.stopping {
                return Err("service is stopping".into());
            }
            if s.active.is_some() && s.phone.as_deref() == Some(device) {
                return Err("正在投屏的 iPhone 不能忘记，先切到另一台".into());
            }
        }
        let path = zbus::zvariant::ObjectPath::try_from(device)?;
        tokio::time::timeout(Duration::from_secs(5), async {
            self.adapter().await?.call::<_, _, ()>("RemoveDevice", &(path,)).await
        })
        .await
        .map_err(|_| "蓝牙没有及时响应")??;
        let mut s = self.state.lock().unwrap();
        if s.phone.as_deref() == Some(device) {
            s.phone = None;
            let _ = std::fs::remove_file(&self.phone_file);
        }
        Ok(())
    }
    /// Close the active phone channel without touching the saved pairing.
    pub async fn disconnect_phone(&self) -> crate::RuntimeResult<()> {
        self.disconnect().await.map_err(|e| match e {
            BtError::Rejected(m) | BtError::Canceled(m) => m.into(),
        })
    }
    /// Power-cycle hci0 through BlueZ. Saved pairings remain; pairing/discovery stay closed.
    pub async fn reset_adapter(&self) -> crate::RuntimeResult<()> {
        let _guard = self.gate.lock().await;
        {
            let mut state = self.state.lock().unwrap();
            if state.stopping {
                return Err("service is stopping".into());
            }
            if state.pairing_open() {
                return Err("pairing is still in progress".into());
            }
        }
        self.disconnect_phone().await?;
        self.set("Powered", false).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.set("Powered", true).await?;
        self.set_name().await?;
        self.set_visible(false).await?;
        Ok(())
    }
    /// Adapter power state as reported by BlueZ, bounded to two seconds.
    pub async fn adapter_powered(&self) -> Option<bool> {
        tokio::time::timeout(Duration::from_secs(2), async {
            self.adapter()
                .await
                .ok()?
                .get_property::<bool>("Powered")
                .await
                .ok()
        })
        .await
        .ok()
        .flatten()
    }
    pub async fn stop(&self) {
        // Set the terminal state before the first await; callbacks and existing HTTP requests cannot reconnect.
        let (target, pairing) = {
            let mut state = self.state.lock().unwrap();
            state.stopping = true;
            let outgoing = state.pairing_stop.take().is_some_and(|stop| {let _ = stop.send(true);true});
            let pairing = state.attempt.as_mut().and_then(|a| {
                let pending = outgoing || a.window.is_open(Instant::now());
                a.window.cancel();
                a.reply.take();
                if pending { a.device.clone() } else { None }
            });
            (state.profile_target.clone(), pairing)
        };
        // 8s leaves the server's 3s and owned-child 3s within procd's 15s termination budget.
        let cleanup = async {
            let _guard = self.gate.lock().await;
            let _ = self.stop_discovery_locked().await;
            let _ = self.disconnect().await;
            if let Some(target) = target {
                let pending = self.state.lock().unwrap().profile_request != ProfileRequest::Idle;
                if pending {
                    let _ = self.cancel_profile_request(&target).await;
                } else if let Ok(p) = self.proxy(&target, "org.bluez.Device1").await {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(2),
                        p.call::<_, _, ()>("DisconnectProfile", &(UUID,)),
                    )
                    .await;
                }
            }
            if let Some(target) = pairing {
                let _ = self.disconnect_device(&target).await;
            }
            if let Ok(p) = self.proxy("/org/bluez", "org.bluez.ProfileManager1").await {
                let _ = p
                    .call::<_, _, ()>(
                        "UnregisterProfile",
                        &(ObjectPath::try_from(PROFILE).unwrap(),),
                    )
                    .await;
            }
            if let Ok(p) = self.proxy("/org/bluez", "org.bluez.AgentManager1").await {
                let _ = p
                    .call::<_, _, ()>("UnregisterAgent", &(ObjectPath::try_from(AGENT).unwrap(),))
                    .await;
            }
            let _ = self
                .connection
                .object_server()
                .remove::<Agent, _>(AGENT)
                .await;
            let _ = self
                .connection
                .object_server()
                .remove::<Profile, _>(PROFILE)
                .await;
        };
        if tokio::time::timeout(Duration::from_secs(8), cleanup)
            .await
            .is_err()
        {
            f50_server::diag_echo!(Info, "bt", "BLUEZ_STOP_DEADLINE; owned daemon shutdown follows");
        }
    }
}
fn finish_confirmation(
    state: &mut State,
    generation: u64,
    device: &str,
    accepted: bool,
) -> std::result::Result<(), BtError> {
    if state.stopping {
        return Err(rejected());
    }
    let current = state
        .attempt
        .as_mut()
        .filter(|a| a.generation == generation && a.device.as_deref() == Some(device));
    if let Some(a) = current {
        if accepted && a.window.phase(Instant::now()) == Phase::Pairing {
            return Ok(());
        }
        a.window.cancel();
        a.reply.take();
    }
    Err(rejected())
}

struct Agent(Weak<Manager>);
#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
        #[zbus(header)] header: Header<'_>,
    ) -> std::result::Result<(), BtError> {
        let m = self.0.upgrade().ok_or_else(rejected)?;
        m.sender(&header)?;
        if !phone_path(device.as_str(), m.adapter_index) { return Err(rejected()); }
        f50_server::diag_log!(Info,"bt","收到蓝牙数字确认回调");
        let (rx, generation) = {
            let mut state = m.state.lock().unwrap();
            if state.stopping {
                f50_server::diag_log!(Warn,"bt","数字确认被拒绝：服务正在停止");
                return Err(rejected());
            }
            let Some(a) = state.attempt.as_mut() else {
                f50_server::diag_log!(Warn,"bt","数字确认被拒绝：没有配对窗口");
                return Err(rejected());
            };
            if a.device.as_deref().is_some_and(|d| d != device.as_str()) {
                f50_server::diag_log!(Warn,"bt","数字确认被拒绝：与当前配对目标不同");
                return Err(rejected());
            }
            if a.window.request_confirmation(Instant::now(), passkey).is_err() {
                f50_server::diag_log!(Warn,"bt","数字确认被拒绝：窗口已结束或已有请求");
                return Err(rejected());
            }
            a.device = Some(device.to_string());
            let (tx, rx) = oneshot::channel();
            a.reply = Some(tx);
            (rx, a.generation)
        };
        f50_server::diag_log!(Info,"bt","收到蓝牙数字确认请求 generation={}；请在 CowPlay 核对六码并确认",generation);
        let accepted = matches!(
            tokio::time::timeout(Duration::from_secs(120), rx).await,
            Ok(Ok(true))
        );
        finish_confirmation(
            &mut m.state.lock().unwrap(),
            generation,
            device.as_str(),
            accepted,
        )
    }
    fn request_pin_code(&self, _device: OwnedObjectPath, #[zbus(header)] header: Header<'_>) -> std::result::Result<String, BtError> {
        let m = self.0.upgrade().ok_or_else(rejected)?; m.sender(&header)?;
        f50_server::diag_log!(Warn,"bt","收到不支持的蓝牙 PIN 请求，已拒绝");
        Err(rejected())
    }
    fn request_passkey(&self, _device: OwnedObjectPath, #[zbus(header)] header: Header<'_>) -> std::result::Result<u32, BtError> {
        let m = self.0.upgrade().ok_or_else(rejected)?; m.sender(&header)?;
        f50_server::diag_log!(Warn,"bt","收到不支持的蓝牙输入码请求，已拒绝");
        Err(rejected())
    }
    fn request_authorization(&self, _device: OwnedObjectPath, #[zbus(header)] header: Header<'_>) -> std::result::Result<(), BtError> {
        let m = self.0.upgrade().ok_or_else(rejected)?; m.sender(&header)?;
        f50_server::diag_log!(Warn,"bt","收到无数字比较的蓝牙授权请求，已拒绝");
        Err(rejected())
    }
    async fn authorize_service(
        &self,
        device: OwnedObjectPath,
        uuid: String,
        #[zbus(header)] header: Header<'_>,
    ) -> std::result::Result<(), BtError> {
        let m = self.0.upgrade().ok_or_else(rejected)?;
        m.sender(&header)?;
        if !phone_path(device.as_str(), m.adapter_index) || uuid != UUID {
            return Err(rejected());
        }
        m.observed_pairing(device.as_str()).await.map_err(|_| rejected())?;
        let state = m.state.lock().unwrap();
        if !state.stopping && state.phone.as_deref() == Some(device.as_str()) {
            Ok(())
        } else {
            Err(rejected())
        }
    }
    fn cancel(&self, #[zbus(header)] header: Header<'_>) -> std::result::Result<(), BtError> {
        let m = self.0.upgrade().ok_or_else(rejected)?;
        m.sender(&header)?;
        if let Some(a) = m.state.lock().unwrap().attempt.as_mut() {
            a.window.cancel();
            a.reply.take();
        }
        f50_server::diag_log!(Info,"bt","蓝牙端取消配对请求");
        Ok(())
    }
    fn release(&self, #[zbus(header)] header: Header<'_>) -> std::result::Result<(), BtError> {
        self.cancel(header)
    }
}
struct Profile(Weak<Manager>);
#[zbus::interface(name = "org.bluez.Profile1")]
impl Profile {
    fn new_connection(
        &self,
        device: OwnedObjectPath,
        fd: OwnedFd,
        _options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
    ) -> std::result::Result<(), BtError> {
        let m = self.0.upgrade().ok_or_else(rejected)?;
        m.sender(&header)?;
        if !phone_path(device.as_str(), m.adapter_index) { return Err(rejected()); }
        let fd: std::os::fd::OwnedFd = fd.into();
        crate::rfcomm::validate(&fd).map_err(|_| rejected())?;
        let mut state = m.state.lock().unwrap();
        if state.stopping
            || state.active.is_some()
            || state.profile_request != ProfileRequest::Opening
            || state.profile_target.as_deref() != Some(device.as_str())
            || state.phone.as_deref() != Some(device.as_str())
        {
            return Err(rejected());
        }
        state.epoch = state.epoch.checked_add(1).ok_or_else(rejected)?;
        let epoch = state.epoch;
        let (stop, rx) = watch::channel(false);
        m.channels
            .try_send(Channel {
                fd,
                epoch,
                selection: state.selection,
                device: device.to_string(),
                stop: rx,
            })
            .map_err(|_| rejected())?;
        state.active = Some(Active {
            epoch,
            stop,
            done: watch::channel(false).0,
        });
        state.profile_request = ProfileRequest::Idle;
        state.profile_target = Some(device.to_string());
        Ok(())
    }
    async fn request_disconnection(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
    ) -> std::result::Result<(), BtError> {
        let m = self.0.upgrade().ok_or_else(rejected)?;
        m.sender(&header)?;
        let matches = m.state.lock().unwrap().phone.as_deref() == Some(device.as_str());
        if matches {
            m.disconnect().await?
        }
        Ok(())
    }
    async fn release(
        &self,
        #[zbus(header)] header: Header<'_>,
    ) -> std::result::Result<(), BtError> {
        let m = self.0.upgrade().ok_or_else(rejected)?;
        m.sender(&header)?;
        m.disconnect().await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stored_device_is_one_adapter_only() {
        assert!(phone_path("/org/bluez/hci0/dev_00_11_22_33_44_55", 0));
        for p in [
            "/org/bluez/hci1/dev_00_11_22_33_44_55",
            "/tmp/x",
            "/org/bluez/hci0/dev_00_11_22_33_44_ZZ",
        ] {
            assert!(!phone_path(p, 0));
        }
    }
}

#[cfg(test)]
mod confirmation_tests {
    use super::*;
    const PHONE: &str = "/org/bluez/hci0/dev_00_11_22_33_44_55";
    fn confirmed() -> State {
        let now = Instant::now();
        let mut window = PairingWindow::new(now);
        let id = window.request_confirmation(now, 42).unwrap();
        window.confirm(now, id, true).unwrap();
        State {
            attempt: Some(Attempt {
                window,
                generation: 7,
                device: Some(PHONE.into()),
                reply: None,
            }),
            generation: 7,
            ..State::default()
        }
    }
    #[test]
    fn accepted_reply_is_rejected_after_cancel() {
        let mut s = confirmed();
        s.attempt.as_mut().unwrap().window.cancel();
        assert!(finish_confirmation(&mut s, 7, PHONE, true).is_err());
    }
    #[test]
    fn accepted_reply_is_rejected_during_shutdown() {
        let mut state = confirmed();
        state.stopping = true;
        assert!(finish_confirmation(&mut state, 7, PHONE, true).is_err());
    }
    #[test]
    fn accepted_reply_cannot_cross_windows_or_devices() {
        let mut s = confirmed();
        assert!(finish_confirmation(&mut s, 6, PHONE, true).is_err());
        assert!(finish_confirmation(&mut s, 7, "/other", true).is_err());
        assert!(finish_confirmation(&mut s, 7, PHONE, true).is_ok());
    }
}

#[cfg(test)]
pub(crate) mod recovery_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    const PHONE: &str = "/org/bluez/hci0/dev_00_11_22_33_44_55";
    const OWNER: &str = ":1.42";
    #[derive(Default)]
    pub(crate) struct Remote {
        pub(crate) connecting: tokio::sync::Notify,
        pub(crate) release: tokio::sync::Notify,
        pub(crate) calls: AtomicUsize,
        pub(crate) disconnects: AtomicUsize,
        pub(crate) fail_disconnect: AtomicBool,
        pub(crate) disconnect_delay_ms: AtomicU64,
        pub(crate) fail_connect: AtomicBool,
        pub(crate) hold_connect_reply: AtomicBool,
        connect_pending: AtomicBool,
        connect_release: tokio::sync::Notify,
        pairing: tokio::sync::Notify,
        pair_calls: AtomicUsize,
        cancel_pairings: AtomicUsize,
        paired: AtomicBool,
        fail_pair: AtomicBool,
        hold_pair_reply: AtomicBool,
        hold_pair_start: AtomicBool,
    }
    struct Device(Arc<Remote>, bool, &'static str, &'static str, Option<Arc<HeldObjects>>);
    #[derive(Default)]
    struct Adapter {pairable:bool,discoverable:bool,discovering:bool,alias:String,transport:String,stored_alias:String,powered_off:bool}
    #[zbus::interface(name = "org.bluez.Adapter1")]
    impl Adapter {
        #[zbus(property)] fn pairable(&self)->bool {self.pairable}
        #[zbus(property)] fn set_pairable(&mut self,value:bool) {self.pairable=value;}
        #[zbus(property)] fn discoverable(&self)->bool {self.discoverable}
        #[zbus(property)] fn set_discoverable(&mut self,value:bool) {self.discoverable=value;}
        #[zbus(property)] fn discovering(&self)->bool {self.discovering}
        #[zbus(property)] fn alias(&self)->&str {&self.alias}
        #[zbus(property)] fn set_alias(&mut self,value:String) {if self.stored_alias!=value {self.alias=value.clone();self.stored_alias=value;}}
        #[zbus(property)] fn name(&self)->&str {"cowplay"}
        #[zbus(property)] fn powered(&self)->bool {!self.powered_off}
        #[zbus(property)] fn set_powered(&mut self,value:bool) {self.powered_off=!value;}
        fn set_discovery_filter(&mut self, filter:HashMap<String,OwnedValue>) {
            self.transport=filter.get("Transport").and_then(|v|<&str>::try_from(v).ok()).unwrap_or("").into();
        }
        fn start_discovery(&mut self) {self.discovering=true;}
        fn stop_discovery(&mut self) {self.discovering=false;}
    }
    #[zbus::interface(name = "org.bluez.Device1")]
    impl Device {
        #[zbus(property)]
        async fn paired(&self) -> bool {
            if let Some(held)=&self.4 {if held.first.swap(false,Ordering::SeqCst) {held.entered.notify_one();held.release.notified().await;}}
            self.1 || self.0.paired.load(Ordering::SeqCst)
        }
        #[zbus(property)] fn alias(&self)->&str {self.2}
        #[zbus(property)] fn icon(&self)->&str {self.3}
        #[zbus(property)] fn connected(&self)->bool {false}
        #[zbus(property)] fn bonded(&self)->bool {self.1 || self.0.paired.load(Ordering::SeqCst)}
        async fn pair(&self, #[zbus(connection)] connection:&Connection, #[zbus(header)] header:Header<'_>)->zbus::fdo::Result<()> {
            self.0.pair_calls.fetch_add(1,Ordering::SeqCst);
            self.0.pairing.notify_one();
            if self.0.hold_pair_start.load(Ordering::SeqCst) {
                self.0.release.notified().await;
                return Err(zbus::fdo::Error::Failed("synthetic cancelled pairing".into()))
            }
            if self.0.fail_pair.load(Ordering::SeqCst) { return Err(zbus::fdo::Error::Failed("synthetic pairing rejection".into())) }
            let agent=Proxy::new(connection,":1.43",AGENT,"org.bluez.Agent1").await.unwrap();
            agent.call::<_,_,()>("RequestConfirmation",&(header.path().unwrap(),42u32)).await
                .map_err(|e|zbus::fdo::Error::Failed(e.to_string()))?;
            self.0.paired.store(true,Ordering::SeqCst);
            if self.0.hold_pair_reply.load(Ordering::SeqCst) {self.0.release.notified().await;}
            Ok(())
        }
        fn cancel_pairing(&self) {
            self.0.cancel_pairings.fetch_add(1,Ordering::SeqCst);
            // BlueZ 5.83 sends UNPAIR when bonding has completed but its Pair reply is
            // still waiting for SDP. Model that side effect instead of a harmless no-op.
            self.0.paired.store(false,Ordering::SeqCst);
            self.0.release.notify_one();
        }
        async fn connect_profile(&self, _uuid: &str) -> zbus::fdo::Result<()> {
            self.0.calls.fetch_add(1, Ordering::SeqCst);
            self.0.connect_pending.store(true, Ordering::SeqCst);
            self.0.connecting.notify_one();
            if self.0.fail_connect.load(Ordering::SeqCst) {
                self.0.connect_pending.store(false, Ordering::SeqCst);
                return Err(zbus::fdo::Error::Failed("synthetic profile connection refused".into()));
            }
            if self.0.hold_connect_reply.load(Ordering::SeqCst) { self.0.connect_release.notified().await; }
            else { self.0.release.notified().await; }
            self.0.connect_pending.store(false, Ordering::SeqCst);
            Ok(())
        }
        async fn disconnect(&self) -> zbus::fdo::Result<()> {
            self.0.disconnects.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(self.0.disconnect_delay_ms.load(Ordering::SeqCst))).await;
            if self.0.fail_disconnect.load(Ordering::SeqCst) {
                return Err(zbus::fdo::Error::Failed(
                    "synthetic cancellation failure".into(),
                ));
            }
            self.0.release.notify_one();
            if self.0.connect_pending.swap(false, Ordering::SeqCst) { self.0.connect_release.notify_one(); }
            Ok(())
        }
    }
    async fn fixture() -> (Arc<Manager>, Connection, Arc<Remote>) {
        fixture_with_timeout(Duration::from_millis(80)).await
    }
    async fn fixture_with_timeout(timeout: Duration) -> (Arc<Manager>, Connection, Arc<Remote>) {
        fixture_on(timeout, 0).await
    }
    async fn fixture_on(timeout: Duration, adapter_index: u16) -> (Arc<Manager>, Connection, Arc<Remote>) {
        let remote = Arc::new(Remote::default());
        let adapter_path = format!("/org/bluez/hci{adapter_index}");
        let phone = format!("{adapter_path}/dev_00_11_22_33_44_55");
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let server = zbus::connection::Builder::unix_stream(a)
            .server("0123456789abcdef0123456789abcdef")
            .unwrap()
            .p2p()
            .unique_name(OWNER)
            .unwrap()
            .serve_at("/",zbus::fdo::ObjectManager).unwrap()
            .serve_at(adapter_path.as_str(),Adapter::default()).unwrap()
            .serve_at(phone.as_str(), Device(remote.clone(),true,"Phone A","phone",None))
            .unwrap()
            .build();
        let client = zbus::connection::Builder::unix_stream(b)
            .p2p()
            .unique_name(":1.43")
            .unwrap()
            .method_timeout(timeout)
            .build();
        let (server, connection) = tokio::try_join!(server, client).unwrap();
        let (channels, _rx) = mpsc::channel(1);
        let manager = Arc::new(Manager {
            connection,
            private_runtime: false,
            controller: controller_name::Controller::mocked(),
            advertised_name: BLUETOOTH_NAME.into(),
            adapter_index,
            adapter_path,
            owner: OWNER.into(),
            state: Mutex::new(State {
                phone: Some(phone),
                ..Default::default()
            }),
            gate: Default::default(),
            phone_file: PathBuf::new(),
            channels,
        });
        (manager, server, remote)
    }
    #[tokio::test]
    async fn opening_pairing_repairs_controller_when_bluez_alias_already_matches() {
        let (manager, server, _remote) = fixture().await;
        let adapter = server.object_server().interface::<_, Adapter>("/org/bluez/hci0").await.unwrap();
        { let mut a=adapter.get_mut().await; a.alias=BLUETOOTH_NAME.into(); a.stored_alias=BLUETOOTH_NAME.into(); }
        let hardware=manager.controller.mock();
        { let mut h=hardware.state.lock().unwrap();h.name="SPREADTRUM BT".into();h.eir=[0;240]; }
        manager.open().await.unwrap();
        let actual=manager.controller.inspect().await.unwrap();
        assert_eq!(actual.controller_name,BLUETOOTH_NAME,"BlueZ Alias cannot substitute for HCI readback");
        assert_eq!(actual.eir_name.as_deref(),Some(BLUETOOTH_NAME));
        assert!(adapter.get().await.discoverable);
        manager.stop().await;
    }
    #[tokio::test]
    async fn device_name_is_shared_by_alias_controller_and_eir_after_reopening() {
        let (mut manager, _server, _remote) = fixture().await;
        let expected = "cowplay-rasp";
        Arc::get_mut(&mut manager).unwrap().advertised_name = expected.into();
        for _ in 0..2 {
            manager.open().await.unwrap();
            let info=manager.adapter_info().await.unwrap();
            assert_eq!(manager.advertised_name(),expected);
            assert_eq!(info.alias,expected);
            assert_eq!(info.controller_name,expected);
            assert_eq!(info.eir_name.as_deref(),Some(expected));
            manager.cancel().await.unwrap();
        }
    }
    #[tokio::test]
    async fn failed_hardware_name_or_eir_keeps_pairing_and_discovery_closed() {
        for opcode in [0x0c13,0x0c52] {
            let (manager, server, _remote)=fixture().await;
            let hardware=manager.controller.mock();
            {let mut h=hardware.state.lock().unwrap();h.name="SPREADTRUM BT".into();h.eir=[0;240];h.reject=Some(opcode);}
            assert!(manager.open().await.is_err());
            let adapter=server.object_server().interface::<_,Adapter>("/org/bluez/hci0").await.unwrap();
            let state=adapter.get().await;
            assert!(!state.pairable && !state.discoverable && !state.discovering);
            assert!(manager.status().adapter_name.is_none());
            manager.stop().await;
        }
    }
    #[tokio::test]
    async fn nonzero_adapter_filters_foreign_devices_and_rejects_their_callbacks() {
        let (manager, server, _remote) = fixture_on(Duration::from_millis(200), 7).await;
        const LOCAL: &str = "/org/bluez/hci7/dev_00_11_22_33_44_55";
        assert_eq!(manager.saved_phones().await.unwrap()[0].id, LOCAL);
        server.object_server().at(PHONE, Device(Arc::new(Remote::default()), true, "Other adapter", "phone", None)).await.unwrap();
        assert!(manager.device_lists().await.unwrap().1.is_empty());
        assert!(manager.select_phone(PHONE, 1).is_err());
        assert_eq!(manager.selected_phone().as_deref(), Some(LOCAL));
        manager.connection.object_server().at(AGENT, Agent(Arc::downgrade(&manager))).await.unwrap();
        manager.open().await.unwrap();
        let proxy = Proxy::new(&server, ":1.43", AGENT, "org.bluez.Agent1").await.unwrap();
        let result = proxy.call::<_, _, ()>("RequestConfirmation", &(ObjectPath::try_from(PHONE).unwrap(), 123456u32)).await;
        assert!(result.is_err());
        assert_eq!(manager.status().request, None);
        manager.cancel().await.unwrap();
    }
    #[tokio::test]
    async fn adapter_reset_reconciles_hardware_before_leaving_it_hidden() {
        let (manager, server, _remote)=fixture().await;
        let hardware=manager.controller.mock();
        {let mut h=hardware.state.lock().unwrap();h.name="SPREADTRUM BT".into();h.eir=[0;240];}
        manager.reset_adapter().await.unwrap();
        let actual=manager.adapter_info().await.unwrap();
        assert_eq!(actual.controller_name,"cowplay");
        assert_eq!(actual.eir_name.as_deref(),Some("cowplay"));
        assert!(!actual.discoverable && !actual.pairable);
        let adapter=server.object_server().interface::<_,Adapter>("/org/bluez/hci0").await.unwrap();
        assert!(!adapter.get().await.powered_off);
        manager.stop().await;
    }
    #[tokio::test]
    async fn normal_bluez_disconnect_delay_does_not_hide_the_profile_failure() {
        let (manager, _server, remote) = fixture_with_timeout(Duration::from_secs(5)).await;
        remote.fail_connect.store(true, Ordering::SeqCst);
        // BlueZ 5.83 waits 2s before asking the controller to disconnect an ACL.
        remote.disconnect_delay_ms.store(2100, Ordering::SeqCst);
        let error = manager.connect().await.unwrap_err().to_string();
        assert!(error.contains("synthetic profile connection refused"), "original failure lost: {error}");
        assert!(manager.state.lock().unwrap().profile_request == ProfileRequest::Idle);
        assert_eq!(remote.disconnects.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn failed_disconnect_keeps_both_causes_and_requires_cleanup_before_retry() {
        let (manager, _server, remote) = fixture().await;
        remote.fail_connect.store(true, Ordering::SeqCst);
        remote.fail_disconnect.store(true, Ordering::SeqCst);
        let error = manager.connect().await.unwrap_err().to_string();
        assert!(error.contains("synthetic profile connection refused"), "original failure lost: {error}");
        assert!(error.contains("synthetic cancellation failure"));
        assert!(manager.state.lock().unwrap().profile_request == ProfileRequest::Cancelling);
        assert!(!error.contains("without an RFCOMM channel"), "failed cleanup must not trigger the no-channel retry");
    }
    const OTHER: &str = "/org/bluez/hci0/dev_00_11_22_33_44_66";
    pub(crate) async fn phones_fixture()->(Arc<Manager>,Connection,Arc<Remote>,Arc<Remote>,Arc<f50_server::Hub>,tempfile::TempDir) {
        let (mut manager,server,a)=fixture().await;
        let b=Arc::new(Remote::default());
        server.object_server().at(OTHER,Device(b.clone(),true,"Phone B","phone",None)).await.unwrap();
        let dir=tempfile::tempdir().unwrap();
        Arc::get_mut(&mut manager).unwrap().phone_file=dir.path().join("phone");
        f50_server::settings::write_private(&manager.phone_file,PHONE.as_bytes()).unwrap();
        let phones=f50_server::phones::Phones::new(None,None);phones.initialize(Some(PHONE)).unwrap();
        let (hub,_commands)=f50_server::Hub::with_phones("123456".into(),"https://test.invalid".into(),phones);
        (manager,server,a,b,hub,dir)
    }
    pub(crate) fn retain_channel(manager: &Manager) -> u64 {
        let mut state = manager.state.lock().unwrap();
        state.epoch += 1;
        let epoch = state.epoch;
        state.active = Some(Active { epoch, stop: watch::channel(false).0, done: watch::channel(false).0 });
        epoch
    }
    #[tokio::test]
    async fn new_pairing_scans_classic_bluetooth_and_exposes_cowplay_name() {
        let (manager,server,_remote)=fixture().await;
        manager.open().await.unwrap();
        let adapter=server.object_server().interface::<_,Adapter>("/org/bluez/hci0").await.unwrap();
        let state=adapter.get().await;
        assert!(state.discovering,"new-phone pairing must actively scan, not only wait for the iPhone");
        assert_eq!(state.transport,"bredr","iAP2 needs classic Bluetooth, not anonymous LE advertisements");
        assert_eq!(state.alias,"cowplay");
        assert!(state.discoverable && state.pairable);
        drop(state);
        manager.cancel().await.unwrap();
        assert!(!adapter.get().await.discovering,"cancel must release the discovery session");
    }
    #[tokio::test]
    async fn stored_alias_does_not_hide_the_vendor_name() {
        let (manager,server,_)=fixture().await;
        let adapter=server.object_server().interface::<_,Adapter>("/org/bluez/hci0").await.unwrap();
        {let mut a=adapter.get_mut().await;a.alias="SPREADTRUM BT".into();a.stored_alias=BLUETOOTH_NAME.into();}
        assert_eq!(manager.adapter_info().await.unwrap().alias,"SPREADTRUM BT");
        assert!(manager.status().adapter_name.is_none());
        manager.open().await.unwrap();
        let info=manager.adapter_info().await.unwrap();
        assert_eq!(info.alias,BLUETOOTH_NAME);
        assert!(info.powered&&info.discoverable&&info.pairable);
        manager.cancel().await.unwrap();
    }
    async fn wait_for(mut condition:impl FnMut()->bool) {
        tokio::time::timeout(Duration::from_secs(1),async {
            while !condition() {tokio::task::yield_now().await;}
        }).await.expect("expected Bluetooth observation did not arrive");
    }
    async fn nearby(manager:&Arc<Manager>,server:&Connection,path:&str,name:&'static str,icon:&'static str)->Arc<Remote> {
        let remote=Arc::new(Remote::default());
        server.object_server().at(path,Device(remote.clone(),false,name,icon,None)).await.unwrap();
        wait_for(||manager.state.lock().unwrap().discovered.contains(path)).await;
        remote
    }
    #[tokio::test]
    async fn nearby_results_are_fresh_and_prioritize_iphones_without_hiding_other_devices() {
        let (manager,server,_)=fixture().await;
        server.object_server().at(OTHER,Device(Arc::new(Remote::default()),false,"stale iPhone","phone",None)).await.unwrap();
        manager.open().await.unwrap();
        assert!(manager.device_lists().await.unwrap().1.is_empty(),"cached objects must not be reported as nearby");
        nearby(&manager,&server,"/org/bluez/hci0/dev_00_11_22_33_44_77","A speaker","audio-headset").await;
        nearby(&manager,&server,"/org/bluez/hci0/dev_00_11_22_33_44_88","B renamed phone","phone").await;
        nearby(&manager,&server,"/org/bluez/hci0/dev_00_11_22_33_44_99","iPhone X","phone").await;
        let devices=manager.device_lists().await.unwrap().1;
        assert_eq!(devices.iter().map(|d|d.name.as_str()).collect::<Vec<_>>(),["iPhone X","B renamed phone","A speaker"]);
        assert!(devices[0].iphone);assert!(!devices[1].iphone);
        assert!(manager.prepare_pair(OTHER,manager.status().generation).await.is_err(),"a stale cached target must not be paired");
        let generation=manager.status().generation;
        assert!(manager.prepare_pair(&devices[0].id,generation+1).await.is_err());
        manager.cancel().await.unwrap();
        manager.open().await.unwrap();
        assert!(manager.device_lists().await.unwrap().1.is_empty(),"a new scan must not reuse last window's sightings");
        manager.cancel().await.unwrap();
    }
    #[tokio::test]
    async fn outbound_pair_keeps_confirmation_live_then_automatically_invites_only_the_chosen_phone() {
        let (manager,server,a,_b,hub,_dir)=phones_fixture().await;
        manager.connection.object_server().at(AGENT,Agent(Arc::downgrade(&manager))).await.unwrap();
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        const TARGET:&str="/org/bluez/hci0/dev_00_11_22_33_44_88";
        let remote=nearby(&manager,&server,TARGET,"iPhone X","phone").await;
        let generation=manager.status().generation;
        tokio::time::timeout(Duration::from_millis(200),crate::phone_flow::pair_device(&manager,&hub,TARGET,generation)).await.unwrap().unwrap();
        wait_for(||manager.status().request.is_some()).await;
        // Fixture's normal RPC timeout is 80ms; Pair must remain live for human confirmation.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let status=manager.status();assert_eq!(status.passkey.as_deref(),Some("000042"));
        assert!(!status.discovering);assert_eq!(status.selected.as_deref(),Some(PHONE));
        assert!(manager.prepare_pair(TARGET,generation).await.is_err(),"duplicate clicks must not issue a second Pair");
        manager.confirm(generation,status.request.unwrap(),true).unwrap();
        tokio::time::timeout(Duration::from_secs(1),remote.connecting.notified()).await.unwrap();
        assert_eq!(remote.pair_calls.load(Ordering::SeqCst),1);
        assert_eq!(remote.calls.load(Ordering::SeqCst),1);
        assert_eq!(a.calls.load(Ordering::SeqCst),0);
        assert_eq!(remote.disconnects.load(Ordering::SeqCst),0,"first invitation must preserve the freshly paired ACL");
        assert_eq!(manager.selected_phone().as_deref(),Some(TARGET));
        assert_eq!(hub.phones.snapshot().device.as_deref(),Some(TARGET));
        let epoch=hub.pause_phone().unwrap();manager.quiesce(epoch).await.unwrap();
    }
    #[tokio::test]
    async fn cancelling_outbound_pair_rejects_late_confirmation_and_preserves_saved_phone() {
        let (manager,server,_a,_b,hub,_dir)=phones_fixture().await;
        manager.connection.object_server().at(AGENT,Agent(Arc::downgrade(&manager))).await.unwrap();
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        let target="/org/bluez/hci0/dev_00_11_22_33_44_88";
        let remote=nearby(&manager,&server,target,"iPhone X","phone").await;
        let generation=manager.status().generation;
        crate::phone_flow::pair_device(&manager,&hub,target,generation).await.unwrap();
        wait_for(||manager.status().request.is_some()).await;
        let request=manager.status().request.unwrap();
        tokio::time::timeout(Duration::from_millis(200),manager.cancel()).await.unwrap().unwrap();
        assert!(manager.confirm(generation,request,true).is_err());
        assert_eq!(remote.cancel_pairings.load(Ordering::SeqCst),0);
        assert_eq!(remote.disconnects.load(Ordering::SeqCst),1);
        assert_eq!(remote.calls.load(Ordering::SeqCst),0);
        assert_eq!(manager.selected_phone().as_deref(),Some(PHONE));
        assert!(manager.status().pairing_error.is_none());
        manager.open().await.unwrap();
        assert!(manager.status().generation>generation);manager.cancel().await.unwrap();
    }
    #[tokio::test]
    async fn outbound_pair_failure_is_visible_without_discarding_old_bonds() {
        let (manager,server,_a,_b,hub,_dir)=phones_fixture().await;
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        let target="/org/bluez/hci0/dev_00_11_22_33_44_88";
        let remote=nearby(&manager,&server,target,"iPhone X","phone").await;
        remote.fail_pair.store(true,Ordering::SeqCst);
        crate::phone_flow::pair_device(&manager,&hub,target,manager.status().generation).await.unwrap();
        wait_for(||manager.status().pairing_error.is_some()).await;
        assert!(manager.status().pairing_error.unwrap().contains("synthetic pairing rejection"));
        assert_eq!(manager.status().phase,"Cancelled");
        assert_eq!(manager.selected_phone().as_deref(),Some(PHONE));
        assert_eq!(remote.calls.load(Ordering::SeqCst),0);
        manager.open().await.unwrap();assert!(manager.status().pairing_error.is_none());manager.cancel().await.unwrap();
    }
    #[tokio::test]
    async fn expiring_a_window_cancels_the_pending_remote_pair_before_retry() {
        let (manager,server,_a,_b,hub,_dir)=phones_fixture().await;
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        let target="/org/bluez/hci0/dev_00_11_22_33_44_88";
        let remote=nearby(&manager,&server,target,"iPhone X","phone").await;
        remote.hold_pair_start.store(true,Ordering::SeqCst);
        crate::phone_flow::pair_device(&manager,&hub,target,manager.status().generation).await.unwrap();
        remote.pairing.notified().await;
        manager.state.lock().unwrap().attempt.as_mut().unwrap().window.deadline=Instant::now();
        // The point-to-point fixture routes unique names; exercise the actual cleanup under
        // the same gate as poll(), independently of the message bus's owner lookup.
        let guard=manager.gate.lock().await;
        manager.poll_pairing_locked().await.unwrap();
        assert_eq!(manager.status().phase,"Expired");
        assert!(manager.state.lock().unwrap().pairing_stop.is_none());
        assert_eq!(remote.cancel_pairings.load(Ordering::SeqCst),0);
        assert_eq!(remote.disconnects.load(Ordering::SeqCst),1);
        assert_eq!(manager.selected_phone().as_deref(),Some(PHONE));
        drop(guard);
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        assert_eq!(manager.status().phase,"Waiting");manager.cancel().await.unwrap();
    }
    #[tokio::test]
    async fn cancelling_current_pairing_preserves_saved_phone_and_allows_new_window() {
        let (manager,_server,_a,_b,hub,_dir)=phones_fixture().await;
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        let generation=manager.status().generation;
        crate::phone_flow::cancel_pairing(&manager,&hub,generation).await.unwrap();
        assert_eq!(manager.selected_phone().as_deref(),Some(PHONE));
        assert!(!manager.pairing_open());
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        assert!(manager.status().generation>generation);
        assert_eq!(manager.status().phase,"Waiting");
    }
    #[tokio::test]
    async fn stale_pairing_generation_cannot_cancel_a_new_window() {
        let (manager,_server,_a,_b,hub,_dir)=phones_fixture().await;
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        let old=manager.status().generation;
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        let epoch=hub.phones.epoch();
        assert!(crate::phone_flow::cancel_pairing(&manager,&hub,old).await.is_err());
        assert_eq!(hub.phones.epoch(),epoch);
        assert_eq!(manager.status().phase,"Waiting");
    }
    #[tokio::test]
    async fn queued_pairing_cancel_cannot_override_a_new_phone_selection() {
        let (manager,_server,_a,_b,hub,_dir)=phones_fixture().await;
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        let generation=manager.status().generation;
        let lock=hub.phones.operation.lock().await;
        let cancel=tokio::spawn({let manager=manager.clone();let hub=hub.clone();async move{crate::phone_flow::cancel_pairing(&manager,&hub,generation).await}});
        tokio::task::yield_now().await;
        let next=hub.pause_phone().unwrap();
        drop(lock);
        assert!(cancel.await.unwrap().is_err());
        assert_eq!(hub.phones.epoch(),next);
        assert_eq!(manager.status().phase,"Waiting");
    }
    #[tokio::test]
    async fn late_pairing_cancel_keeps_an_established_carplay_session() {
        let (manager,_server,_a,_b,hub,_dir)=phones_fixture().await;
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        hub.phone_active.store(true,Ordering::Release);
        let epoch=hub.phones.epoch();
        let _result=crate::phone_flow::cancel_pairing(&manager,&hub,manager.status().generation).await;
        assert_eq!(hub.phones.epoch(),epoch,"a late cancel must not pause the completed CarPlay session");
        assert!(hub.phone_active.load(Ordering::Acquire));
    }
    #[tokio::test]
    async fn cancel_after_pair_success_revokes_auto_connection_waiting_for_device_list() {
        let (manager,server,_a,_b,hub,_dir)=phones_fixture().await;
        manager.connection.object_server().at(AGENT,Agent(Arc::downgrade(&manager))).await.unwrap();
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        let target="/org/bluez/hci0/dev_00_11_22_33_44_88";
        let remote=nearby(&manager,&server,target,"iPhone X","phone").await;
        crate::phone_flow::pair_device(&manager,&hub,target,manager.status().generation).await.unwrap();
        wait_for(||manager.status().request.is_some()).await;
        let held=hold_first_list(&server).await;
        let status=manager.status();manager.confirm(status.generation,status.request.unwrap(),true).unwrap();
        tokio::time::timeout(Duration::from_secs(1),held.entered.notified()).await.unwrap();
        assert_eq!(manager.status().phase,"Paired");
        let previous=hub.phones.epoch();
        crate::phone_flow::cancel_pairing(&manager,&hub,manager.status().generation).await.unwrap();
        assert!(hub.phones.epoch()>previous);
        held.release.notify_one();
        // Drain the delayed request; a cancelled generation must not make ConnectProfile.
        manager.validate_phone(target).await.unwrap();
        assert_eq!(remote.calls.load(Ordering::SeqCst),0);
        assert!(!hub.phones.snapshot().admission);
    }
    /// Simulate BlueZ asking the agent to compare digits for an iPhone that chose cowplay itself.
    async fn incoming_pair(manager:&Arc<Manager>,server:&Connection,target:&'static str,remote:&Arc<Remote>) {
        let agent=Proxy::new(server,":1.43",AGENT,"org.bluez.Agent1").await.unwrap();
        let path=OwnedObjectPath::try_from(target).unwrap();
        let call=tokio::spawn(async move {agent.call::<_,_,()>("RequestConfirmation",&(path,42u32)).await});
        wait_for(||manager.status().request.is_some()).await;
        let status=manager.status();assert_eq!(status.pairing_device.as_deref(),Some(target));
        manager.confirm(status.generation,status.request.unwrap(),true).unwrap();
        call.await.unwrap().unwrap();
        remote.paired.store(true,Ordering::SeqCst);
        let guard=manager.gate.lock().await;
        manager.poll_pairing_locked().await.unwrap();
        drop(guard);
        assert_eq!(manager.status().phase,"Paired");
    }
    #[tokio::test]
    async fn iphone_initiated_pairing_invites_only_the_new_phone_once() {
        let (manager,server,a,b,hub,_dir)=phones_fixture().await;
        manager.connection.object_server().at(AGENT,Agent(Arc::downgrade(&manager))).await.unwrap();
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        const TARGET:&str="/org/bluez/hci0/dev_00_11_22_33_44_88";
        let remote=nearby(&manager,&server,TARGET,"iPhone X","phone").await;
        assert_eq!(manager.take_new_pairing(),None,"nothing is handed off before pairing completes");
        incoming_pair(&manager,&server,TARGET,&remote).await;
        assert_eq!(manager.fresh_pairing().as_deref(),Some(TARGET),"a default connect must defer to the new phone");
        assert_eq!(manager.take_new_pairing().as_deref(),Some(TARGET));
        assert_eq!(manager.take_new_pairing(),None,"the service loop starts the invitation once");
        let task=tokio::spawn({let (m,h)=(manager.clone(),hub.clone());let epoch=hub.phones.epoch();
            async move {crate::phone_flow::connect_new_pairing(&m,&h,TARGET,epoch).await}});
        tokio::time::timeout(Duration::from_secs(1),remote.connecting.notified()).await.unwrap();
        assert_eq!(remote.calls.load(Ordering::SeqCst),1);
        assert_eq!(a.calls.load(Ordering::SeqCst)+b.calls.load(Ordering::SeqCst),0,"older phones are not called");
        assert_eq!(manager.selected_phone().as_deref(),Some(TARGET));
        assert_eq!(manager.fresh_pairing().as_deref(),Some(TARGET),"still guarded while the invitation runs");
        assert_eq!(remote.disconnects.load(Ordering::SeqCst),0,"incoming pairing must preserve its ACL");
        // A newer explicit choice supersedes the invitation and releases the guard.
        let epoch=hub.pause_phone().unwrap();manager.quiesce(epoch).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2),task).await.unwrap().unwrap();
        assert_eq!(manager.fresh_pairing(),None);
    }
    #[tokio::test]
    async fn cancelling_after_iphone_initiated_pairing_drops_the_pending_invitation() {
        let (manager,server,a,_b,hub,_dir)=phones_fixture().await;
        manager.connection.object_server().at(AGENT,Agent(Arc::downgrade(&manager))).await.unwrap();
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        const TARGET:&str="/org/bluez/hci0/dev_00_11_22_33_44_88";
        let remote=nearby(&manager,&server,TARGET,"iPhone X","phone").await;
        incoming_pair(&manager,&server,TARGET,&remote).await;
        crate::phone_flow::disconnect(&manager,&hub).await.unwrap();
        assert_eq!(manager.take_new_pairing(),None,"the user's disconnect wins over the pending invitation");
        assert_eq!(manager.fresh_pairing(),None);
        assert_eq!(remote.calls.load(Ordering::SeqCst)+a.calls.load(Ordering::SeqCst),0);
        assert!(manager.saved_phones().await.unwrap().iter().any(|p|p.id==TARGET),"the new bond is kept");
    }
    #[tokio::test]
    async fn cancelling_pair_after_bond_exists_preserves_the_controller_key() {
        let (manager,server,_a,_b,hub,_dir)=phones_fixture().await;
        manager.connection.object_server().at(AGENT,Agent(Arc::downgrade(&manager))).await.unwrap();
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        let target="/org/bluez/hci0/dev_00_11_22_33_44_88";
        let remote=nearby(&manager,&server,target,"iPhone X","phone").await;
        remote.hold_pair_reply.store(true,Ordering::SeqCst);
        crate::phone_flow::pair_device(&manager,&hub,target,manager.status().generation).await.unwrap();
        wait_for(||manager.status().request.is_some()).await;
        let status=manager.status();manager.confirm(status.generation,status.request.unwrap(),true).unwrap();
        wait_for(||remote.paired.load(Ordering::SeqCst)).await;
        manager.cancel().await.unwrap();
        assert!(remote.paired.load(Ordering::SeqCst),"cancel must preserve a completed bond while Pair still awaits service discovery");
        assert_eq!(remote.cancel_pairings.load(Ordering::SeqCst),0,"CancelPairing can issue UNPAIR on BlueZ 5.83");
        assert_eq!(remote.disconnects.load(Ordering::SeqCst),1);
    }
    #[tokio::test]
    async fn outbound_pair_is_not_published_before_the_pair_reply_finishes() {
        let (manager,server,_a,_b,hub,_dir)=phones_fixture().await;
        manager.connection.object_server().at(AGENT,Agent(Arc::downgrade(&manager))).await.unwrap();
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();
        let target="/org/bluez/hci0/dev_00_11_22_33_44_88";
        let remote=nearby(&manager,&server,target,"iPhone X","phone").await;
        remote.hold_pair_reply.store(true,Ordering::SeqCst);
        crate::phone_flow::pair_device(&manager,&hub,target,manager.status().generation).await.unwrap();
        wait_for(||manager.status().request.is_some()).await;
        let status=manager.status();manager.confirm(status.generation,status.request.unwrap(),true).unwrap();
        wait_for(||remote.paired.load(Ordering::SeqCst)).await;
        assert!(!manager.observed_pairing(target).await.unwrap(),"Paired/Bonded alone must not publish a pending outbound Pair RPC");
        assert_eq!(manager.selected_phone().as_deref(),Some(PHONE));
        assert_eq!(manager.status().phase,"Pairing");
        manager.cancel().await.unwrap();
    }
    #[tokio::test]
    async fn saved_phone_list_excludes_nonphones_and_unpaired_devices() {
        let (manager,server,_,_,_,_dir)=phones_fixture().await;
        for (path,paired,icon) in [("/org/bluez/hci0/dev_00_11_22_33_44_77",true,"audio-headset"),("/org/bluez/hci0/dev_00_11_22_33_44_88",false,"phone"),("/org/bluez/hci1/dev_00_11_22_33_44_99",true,"phone")] {
            server.object_server().at(path,Device(Arc::new(Remote::default()),paired,"excluded",icon,None)).await.unwrap();
        }
        let devices=manager.saved_phones().await.unwrap();
        assert_eq!(devices.len(),2);assert_eq!(devices[0].id,PHONE);assert!(devices[0].selected);assert_eq!(devices[1].id,OTHER);
        assert!(manager.validate_phone(OTHER).await.is_ok());
        assert!(manager.validate_phone("/org/bluez/hci0/dev_00_11_22_33_44_88").await.is_err());
    }
    #[tokio::test]
    async fn explicit_switch_stops_a_before_inviting_b_and_releases_cancelled_request() {
        let (manager,_server,a,b,hub,_dir)=phones_fixture().await;
        let switching=tokio::spawn({let m=manager.clone();let h=hub.clone();async move {crate::phone_flow::connect(&m,&h,Some(OTHER)).await}});
        b.connecting.notified().await;
        assert_eq!(a.disconnects.load(Ordering::SeqCst),1,"old Bluetooth connection must close before B invite");
        assert_eq!(b.calls.load(Ordering::SeqCst),1);assert_eq!(manager.selected_phone().as_deref(),Some(OTHER));
        assert_eq!(hub.phones.snapshot().device.as_deref(),Some(OTHER));assert!(!hub.phones.snapshot().admission);
        assert_eq!(std::fs::read_to_string(&manager.phone_file).unwrap(),OTHER);
        let epoch=hub.pause_phone().unwrap();
        assert!(switching.await.unwrap().is_err(),"late old RPC must not finish a superseded selection");
        manager.quiesce(epoch).await.unwrap();
        assert_eq!(b.disconnects.load(Ordering::SeqCst),1);assert!(manager.state.lock().unwrap().profile_request==ProfileRequest::Idle);
        assert_eq!(manager.selected_phone().as_deref(),Some(OTHER),"cancelling an invite preserves selection and bond");
    }
    #[tokio::test]
    async fn invalid_target_and_failed_storage_do_not_publish_a_new_phone() {
        let (manager,_server,a,b,hub,_dir)=phones_fixture().await;
        let epoch=hub.phones.epoch();
        assert!(crate::phone_flow::connect(&manager,&hub,Some("/tmp/file")).await.is_err());
        assert_eq!(hub.phones.epoch(),epoch);assert_eq!(a.disconnects.load(Ordering::SeqCst),0);
        std::fs::remove_file(&manager.phone_file).unwrap();std::fs::create_dir(&manager.phone_file).unwrap();
        assert!(crate::phone_flow::connect(&manager,&hub,Some(OTHER)).await.is_err());
        assert_eq!(manager.selected_phone().as_deref(),Some(PHONE));assert_eq!(hub.phones.snapshot().device.as_deref(),Some(PHONE));
        assert_eq!(b.calls.load(Ordering::SeqCst),0);
    }
    #[tokio::test]
    async fn latest_explicit_selection_supersedes_new_pairing_and_closes_old_channel() {
        let (manager,_server,a,_b,hub,_dir)=phones_fixture().await;
        // Hold the shared operation while a new-phone request revokes old admission.
        hub.phone_verified.store(true,Ordering::Release);
        let guard=hub.phones.operation.lock().await;
        let pairing=tokio::spawn({let m=manager.clone();let h=hub.clone();async move {crate::phone_flow::pair_new(&m,&h).await}});
        while hub.phones.epoch()==0 {tokio::task::yield_now().await;}
        let explicit=tokio::spawn({let m=manager.clone();let h=hub.clone();async move {crate::phone_flow::connect(&m,&h,Some(PHONE)).await}});
        while hub.phones.epoch()==1 {tokio::task::yield_now().await;}
        drop(guard);
        assert!(pairing.await.unwrap().is_err());
        a.connecting.notified().await;
        assert!(!matches!(manager.status().phase,"Waiting"|"Pairing"));
        hub.pause_phone().unwrap();assert!(explicit.await.unwrap().is_err());
    }
    #[tokio::test]
    async fn pairing_another_phone_waits_for_old_channel_close_and_preserves_bond() {
        let (manager,_server,_a,_b,hub,_dir)=phones_fixture().await;
        let (stop,mut stopping)=watch::channel(false);
        manager.state.lock().unwrap().active=Some(Active{epoch:1,stop,done:watch::channel(false).0});
        let old=tokio::spawn({let m=manager.clone();async move{stopping.changed().await.unwrap();assert!(*stopping.borrow());m.complete(1);}});
        crate::phone_flow::pair_new(&manager,&hub).await.unwrap();old.await.unwrap();
        assert_eq!(manager.status().phase,"Waiting");assert!(!manager.status().rfcomm);assert_eq!(manager.selected_phone().as_deref(),Some(PHONE));
        assert!(!hub.phones.snapshot().admission,"old network admission stays closed while pairing a new phone");
        assert!(crate::phone_flow::connect(&manager,&hub,None).await.is_err(),"background viewer retry must not cancel new pairing");
        assert_eq!(manager.status().phase,"Waiting");
    }
    #[derive(Default)]
    struct HeldObjects {first:AtomicBool,entered:tokio::sync::Notify,release:tokio::sync::Notify}
    async fn hold_first_list(server:&Connection)->Arc<HeldObjects> {
        let held=Arc::new(HeldObjects{first:AtomicBool::new(true),..Default::default()});
        let device=server.object_server().interface::<_,Device>(PHONE).await.unwrap();
        device.get_mut().await.4=Some(held.clone());held
    }
    #[tokio::test]
    async fn delayed_default_reconnect_cannot_override_a_new_explicit_selection() {
        let (manager,server,a,b,hub,_dir)=phones_fixture().await;
        assert!(hub.phones.handoff(0,PHONE));
        let held=hold_first_list(&server).await;
        let old=tokio::spawn({let m=manager.clone();let h=hub.clone();async move{crate::phone_flow::connect(&m,&h,None).await}});
        held.entered.notified().await;
        let switch=tokio::spawn({let m=manager.clone();let h=hub.clone();async move{crate::phone_flow::connect(&m,&h,Some(OTHER)).await}});
        b.connecting.notified().await;let epoch=hub.phones.epoch();
        held.release.notify_one();assert!(old.await.unwrap().is_err());
        assert_eq!(hub.phones.epoch(),epoch);assert_eq!(manager.selected_phone().as_deref(),Some(OTHER));
        assert_eq!(a.calls.load(Ordering::SeqCst),0);assert_eq!(b.calls.load(Ordering::SeqCst),1);
        hub.pause_phone().unwrap();assert!(switch.await.unwrap().is_err());
    }
    #[tokio::test]
    async fn switch_cancels_a_phone_still_waiting_for_connect_profile() {
        let (manager,_server,a,b,hub,_dir)=phones_fixture().await;
        let old=tokio::spawn({let m=manager.clone();let h=hub.clone();async move{crate::phone_flow::connect(&m,&h,Some(PHONE)).await}});
        a.connecting.notified().await;
        let switch=tokio::spawn({let m=manager.clone();let h=hub.clone();async move{crate::phone_flow::connect(&m,&h,Some(OTHER)).await}});
        tokio::time::timeout(Duration::from_secs(1),b.connecting.notified()).await.unwrap();
        assert!(old.await.unwrap().unwrap_err().to_string().contains("新的手机选择"));
        assert_eq!(manager.selected_phone().as_deref(),Some(OTHER));
        assert_eq!(a.calls.load(Ordering::SeqCst),1);assert_eq!(b.calls.load(Ordering::SeqCst),1);
        assert!(a.disconnects.load(Ordering::SeqCst)>0);
        hub.pause_phone().unwrap();assert!(switch.await.unwrap().is_err());
    }
    #[tokio::test]
    async fn status_uses_one_selection_after_a_delayed_device_list() {
        let (manager,server,_a,_b,hub,_dir)=phones_fixture().await;
        let held=hold_first_list(&server).await;
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
        let cookie=format!("{}={}",f50_server::clients::COOKIE,hub.clients.admit_token(&hub.clients.token(),"phone","","","").unwrap().id);
        let router=crate::web::router(manager.clone(),hub);
        let serving=tokio::spawn(async move {axum::serve(listener,router).await.unwrap();});
        let request=tokio::spawn(async move {reqwest::Client::new().get(format!("http://{address}/api/pairing")).header("cookie",cookie).send().await.unwrap().text().await.unwrap()});
        held.entered.notified().await;manager.state.lock().unwrap().phone=Some(OTHER.into());held.release.notify_one();
        let status:serde_json::Value=serde_json::from_str(&request.await.unwrap()).unwrap();
        assert_eq!(status["selected"],OTHER);
        let chosen:Vec<_>=status["devices"].as_array().unwrap().iter().filter(|p|p["selected"]==true).collect();
        assert_eq!(chosen.len(),1);assert_eq!(chosen[0]["id"],OTHER);serving.abort();let _=serving.await;
    }
    pub(crate) async fn admin_fixture()->(Arc<Manager>,Connection,Arc<f50_server::Hub>,tempfile::TempDir) {
        let (manager,server,_a,_b,hub,dir)=phones_fixture().await;(manager,server,hub,dir)
    }
    #[tokio::test]
    async fn automatic_connect_waiting_for_phone_list_cannot_replace_root_reset() {
        let (manager,server,_a,_b,hub,dir)=phones_fixture().await;
        let a=crate::admin::Admin::new(manager,hub.clone(),"https://test.invalid".into(),dir.path().join("cowplay.uci"),dir.path().join("config"),dir.path().to_owned(),"cert".into(),"key".into(),"127.0.0.1:8080".parse().unwrap(),"127.0.0.1:7000".parse().unwrap(),None,(800,480)).expect("valid synthetic policy");
        let held=hold_first_list(&server).await;
        let automatic=tokio::spawn({let a=a.clone();async move {crate::console::run_cmd(&a,crate::console::Cmd{cmd:"connect".into(),id:None,generation:None}).await}});
        tokio::time::timeout(Duration::from_secs(1),held.entered.notified()).await.unwrap();
        let before=hub.phones.epoch();
        let reset=tokio::spawn({let a=a.clone();async move {crate::ctl::dispatch(&a,"bluetooth_reset",&serde_json::json!({})).await}});
        wait_for(||hub.phones.epoch()>before).await;
        let resetting=hub.phones.epoch();
        held.release.notify_one();
        automatic.await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(hub.phones.epoch(),resetting,"a delayed automatic phone lookup cannot become an explicit selection");
        assert_eq!(reset.await.unwrap().unwrap()["ok"],true);
        assert!(a.stopped.load(Ordering::Acquire));
    }
    /// The saved phone takes `ms` to disconnect; the closure reports its Disconnect calls.
    pub(crate) async fn admin_fixture_slow_disconnect(ms:u64)->(Arc<Manager>,Connection,Arc<f50_server::Hub>,tempfile::TempDir,impl Fn()->usize) {
        let (manager,server,a,_b,hub,dir)=phones_fixture().await;
        a.disconnect_delay_ms.store(ms,Ordering::SeqCst);
        (manager,server,hub,dir,move||a.disconnects.load(Ordering::SeqCst))
    }
    #[tokio::test]
    async fn pairing_is_published_only_after_private_storage_succeeds_and_can_retry() {
        let (mut manager, _server, _) = fixture().await;
        let dir = tempfile::tempdir().unwrap();
        let phone_file = dir.path().join("missing").join("phone");
        Arc::get_mut(&mut manager).unwrap().phone_file = phone_file.clone();
        let now = Instant::now();
        let mut window = PairingWindow::new(now);
        let request = window.request_confirmation(now, 42).unwrap();
        window.confirm(now, request, true).unwrap();
        {
            let mut s = manager.state.lock().unwrap();
            s.phone = None;
            s.attempt = Some(Attempt { window, generation: 7, device: Some(PHONE.into()), reply: None });
        }
        assert!(manager.observed_pairing(PHONE).await.is_err());
        assert!(!manager.status().paired, "failed write must not announce a saved phone");
        assert_eq!(manager.status().phase, "Pairing", "the confirmed attempt remains retryable");
        std::fs::create_dir(phone_file.parent().unwrap()).unwrap();
        assert!(manager.observed_pairing(PHONE).await.unwrap());
        assert!(manager.status().paired);
        assert_eq!(manager.status().phase, "Paired");
        assert_eq!(std::fs::read_to_string(&phone_file).unwrap(), PHONE);
    }
    #[tokio::test]
    async fn timed_out_connect_cancels_remote_request_before_retry() {
        let (manager, _server, remote) = fixture().await;
        let outcome = manager.connect().await;
        assert!(outcome.is_err());
        assert_eq!(
            remote.disconnects.load(Ordering::SeqCst),
            1,
            "a local RPC timeout must cancel the still-pending BlueZ operation"
        );
        assert!(
            manager.state.lock().unwrap().profile_target.is_none(),
            "a failed attempt must not keep permission for a late channel"
        );
        assert!(manager.status().paired, "recovery must preserve pairing");
    }
    #[tokio::test]
    async fn pairing_poll_does_not_block_channel_consumption_during_connect() {
        let (manager, _server, remote) = fixture().await;
        let connecting = tokio::spawn({
            let manager = manager.clone();
            async move { manager.connect().await }
        });
        remote.connecting.notified().await;
        let polled = tokio::time::timeout(Duration::from_millis(20), manager.poll()).await;
        remote.release.notify_one();
        let _ = connecting.await.unwrap();
        assert!(
            matches!(polled, Ok(Ok(()))),
            "the service must remain able to consume the arriving RFCOMM FD while ConnectProfile waits"
        );
    }
    #[tokio::test]
    async fn aborted_connect_retains_remote_cleanup_for_the_next_attempt() {
        let (manager, _server, remote) = fixture().await;
        let connecting = tokio::spawn({
            let m = manager.clone();
            async move { m.connect().await }
        });
        remote.connecting.notified().await;
        connecting.abort();
        assert!(connecting.await.unwrap_err().is_cancelled());
        assert!(
            manager.state.lock().unwrap().profile_request == ProfileRequest::Cancelling,
            "dropping a local future must not leave permission for a late BlueZ channel"
        );
        assert!(manager.status().paired);
    }
    #[tokio::test]
    async fn failed_cleanup_is_retried_before_starting_another_remote_connect() {
        let (manager, _server, remote) = fixture().await;
        remote.fail_disconnect.store(true, Ordering::SeqCst);
        assert!(manager.connect().await.is_err());
        assert!(manager.state.lock().unwrap().profile_request == ProfileRequest::Cancelling);
        assert!(
            manager.open().await.is_err(),
            "pairing cannot overlap unresolved cleanup"
        );
        remote.fail_disconnect.store(false, Ordering::SeqCst);
        // Consume the first request's notification before waiting for the next one.
        remote.connecting.notified().await;
        let retry = tokio::spawn({
            let m = manager.clone();
            async move { m.connect().await }
        });
        remote.connecting.notified().await;
        assert_eq!(remote.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            remote.disconnects.load(Ordering::SeqCst),
            2,
            "the old request must be cancelled before issuing the second connect"
        );
        remote.release.notify_one();
        let _ = retry.await.unwrap();
        assert!(manager.status().paired);
    }
    #[tokio::test]
    async fn confirmed_channel_survives_a_lost_reply_and_repeated_connect_is_idempotent() {
        let (manager, _server, remote) = fixture().await;
        let connecting = tokio::spawn({
            let m = manager.clone();
            async move { m.connect().await }
        });
        remote.connecting.notified().await;
        // Model the state established by an accepted Profile1 FD, without simulating media.
        manager.state.lock().unwrap().active = Some(Active {
            epoch: 1,
            stop: watch::channel(false).0,
            done: watch::channel(false).0,
        });
        assert!(
            connecting.await.unwrap().is_ok(),
            "actual channel state outranks the RPC timeout"
        );
        assert!(manager.connect().await.is_ok());
        assert_eq!(remote.calls.load(Ordering::SeqCst), 1);
        assert_eq!(remote.disconnects.load(Ordering::SeqCst), 0);
        manager.complete(1);
        assert!(!manager.status().rfcomm);
        assert!(manager.state.lock().unwrap().profile_target.is_none());
    }
    #[tokio::test]
    async fn saved_phone_connect_does_not_overlap_a_new_pairing_window() {
        let (manager, _server, remote) = fixture().await;
        manager.state.lock().unwrap().attempt = Some(Attempt {
            window: PairingWindow::new(Instant::now()),
            generation: 1,
            device: None,
            reply: None,
        });
        assert!(manager.connect().await.is_err());
        assert_eq!(remote.calls.load(Ordering::SeqCst), 0);
        assert!(manager.status().paired);
    }
    #[tokio::test]
    async fn service_stop_cancels_an_aborted_pending_connect_without_unpairing() {
        let (manager, _server, remote) = fixture().await;
        let connecting = tokio::spawn({
            let m = manager.clone();
            async move { m.connect().await }
        });
        remote.connecting.notified().await;
        connecting.abort();
        let _ = connecting.await;
        manager.stop().await;
        assert_eq!(
            remote.disconnects.load(Ordering::SeqCst),
            1,
            "service shutdown must cancel a pending BlueZ request, not only disconnect a profile"
        );
        assert!(manager.status().paired);
        assert!(manager.connect().await.is_err());
    }
}
