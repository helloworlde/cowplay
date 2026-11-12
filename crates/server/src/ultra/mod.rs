//! Ultra live vehicle state. All numeric readings originate from the shared authenticated feed.
//! COWPLAY_ULTRA_PROBE=0 retains the existing runtime disable behavior.
mod livi;
pub use crate::vehicle as feed;

use f50_core::{bplist, crypto::{derive, ControlCipher}};
use livi::vehicle_state::{Lane, VehicleState};
use std::{collections::BTreeMap, io, net::IpAddr, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, sync::{mpsc, oneshot, watch}};
use f50_core::vehicle::ClimateCommand;

pub const FEATURE: &str = "vehicleStateProtocol";
const MAX_PACKAGE: usize = 262_144;
const ASYNC: u32 = 0x6173_796e;
const SYNC: u32 = 0x7379_6e63;
const REPLY: u32 = 0x7270_6c79;
const COMMAND: u32 = 0x636d_6e64;

pub fn enabled() -> bool {
    std::env::var_os("COWPLAY_ULTRA_PROBE").as_deref() != Some(std::ffi::OsStr::new("0"))
}

fn failure(message: impl std::fmt::Debug) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{message:?}"))
}

fn live_vehicle() -> VehicleState {
    VehicleState::new(false,"CowPlay Tesla BLE",env!("CARGO_PKG_VERSION")).live_profile()
}

pub struct Probe {
    state: Mutex<VehicleState>,
    outgoing: Mutex<BTreeMap<u8, mpsc::Sender<Vec<u8>>>>,
    feed:Arc<feed::Feed>,
    native: watch::Sender<Option<u64>>,
    native_busy: Arc<AtomicBool>,
    native_replies: Mutex<BTreeMap<i64, (u64, Vec<u8>, Vec<u8>)>>,
    climate_feedback: Mutex<(u64, u64, &'static str)>,
}
struct NativePending {
    request: Option<(i64, Vec<u8>)>,
    reply: oneshot::Receiver<bool>,
    context: u64,
    deadline: tokio::time::Instant,
    grant: watch::Receiver<Option<u64>>,
    busy: Arc<AtomicBool>,
}
impl Drop for NativePending {
    fn drop(&mut self) { self.busy.store(false, Ordering::Release); }
}

struct Registration {
    probe: Arc<Probe>,
    lane: u8,
    sender: mpsc::Sender<Vec<u8>>,
}
impl Drop for Registration {
    fn drop(&mut self) {
        let mut registered = self.probe.outgoing.lock().unwrap();
        if registered.get(&self.lane).is_some_and(|current| current.same_channel(&self.sender)) {
            registered.remove(&self.lane);
            self.probe.revoke_climate_control();
        }
    }
}

impl Probe {
    pub fn connected_channels(&self) -> [bool; 2] {
        let outgoing = self.outgoing.lock().unwrap();
        [outgoing.contains_key(&0), outgoing.contains_key(&1)]
    }
    pub fn new() -> Arc<Self> {
        Self::with_feed(feed::Feed::new())
    }
    pub fn with_feed(feed:Arc<feed::Feed>)->Arc<Self>{
        let mut state=live_vehicle();state.set_live_values(&feed.snapshot().values);
        state.set_live_climate(feed.climate().current());
        let (native, _) = watch::channel(None);
        Arc::new(Self {state:Mutex::new(state),outgoing:Mutex::new(BTreeMap::new()),feed,native,native_busy:Arc::new(AtomicBool::new(false)),native_replies:Mutex::new(BTreeMap::new()),climate_feedback:Mutex::new((0,0,""))})
    }
    pub fn climate_control(&self) -> serde_json::Value {
        let snapshot = self.feed.climate();
        let (sequence, at_ms, message) = *self.climate_feedback.lock().unwrap();
        serde_json::json!({"context":snapshot.context,"available":snapshot.current().is_some(),"allowed":*self.native.borrow() == Some(snapshot.context),"feedback":{"sequence":sequence,"at_ms":at_ms,"age_ms":feed::now_ms().saturating_sub(at_ms),"message":message}})
    }
    fn climate_notice(&self, message: &'static str) {
        let mut feedback = self.climate_feedback.lock().unwrap();
        *feedback = (feedback.0.wrapping_add(1), feed::now_ms(), message);
    }
    pub fn revoke_climate_control(&self) { self.native.send_replace(None); }
    pub fn set_climate_control(&self, enabled: bool, context: u64) -> Result<(), &'static str> {
        let snapshot = self.feed.climate();
        if context != snapshot.context { return Err("车辆上下文已变化，请刷新后重新确认"); }
        if enabled && (snapshot.current().is_none() || self.outgoing.lock().unwrap().is_empty()) {
            return Err("请先建立 Ultra 通道，并读取新鲜的车辆空调状态");
        }
        self.native.send_replace(enabled.then_some(snapshot.context));
        Ok(())
    }
    fn refresh(&self)->io::Result<()> {
        let mut state=self.state.lock().unwrap();let snapshot=self.feed.snapshot();let mut updates=state.set_live_values(&snapshot.values);
        let climate = self.feed.climate();
        let revoked = self.native.borrow().is_some_and(|context| context != climate.context);
        if revoked { self.native.send_replace(None); }
        updates.extend(state.set_live_climate(climate.current()));
        let valid=usize::from(snapshot.values.soc_pct.is_some())+usize::from(snapshot.values.range_km.is_some())+snapshot.values.tires_kpa.iter().filter(|v|v.is_some()).count();
        let phase=match snapshot.phase {"connecting"=>"寻找车辆","authorizing"=>"验证车辆授权","not_authorized"=>"车辆尚未授权本机公钥","configuration_error"=>"配置或密钥不可用","private_bus_unavailable"=>"私有蓝牙服务不可用","ble_scan_failed"=>"未发现车辆或扫描不可用","ble_connect_failed"=>"车辆蓝牙连接失败","ble_gatt_failed"=>"车辆数据通道不可用","authorization_failed"=>"车辆授权验证失败","charge_read_failed"=>"电量与续航读取失败","tire_read_failed"=>"胎压读取失败","cleanup_failed"=>"数据通道释放未确认，已停止重试","read_attempts_exhausted"=>"读取次数已耗尽，请重试","stopped"=>"读取已停止",_=>"车辆未连接"};
        let status=if !snapshot.enabled {"未配置车辆；没有实时读数".to_owned()}else if !snapshot.connected {format!("{phase}；旧读数已失效")}else{format!("已连接车辆；有效读数 {valid}/6；{}",match snapshot.phase {"charge_paused"=>"电量与续航读取已暂停，请重试","tires_paused"=>"胎压读取已暂停，请重试","reads_paused"=>"读取已暂停，请重试",_=>"缺失、采样年龄未知或过期值不可用"})};
        updates.extend(state.set_data_status(&status));
        let outgoing=self.outgoing.lock().unwrap();
        for (lane,message) in updates {if let Some(sender)=outgoing.get(&if lane==Lane::High {0}else{1}).or_else(||outgoing.get(&1)).or_else(||outgoing.get(&0)) {sender.try_send(message).map_err(failure)?;}}
        Ok(())
    }

    pub fn lane(uuid: &str) -> Option<u8> {
        if uuid.eq_ignore_ascii_case(livi::vehicle_state::HIGH_PRIORITY_DATASTREAM_UUID) { Some(0) }
        else if uuid.eq_ignore_ascii_case(livi::vehicle_state::DATASTREAM_UUID) { Some(1) }
        else { None }
    }

    pub fn augment_info(&self, bytes: &[u8]) -> f50_core::Result<Vec<u8>> {
        let (name, description) = self.state.lock().unwrap().info_entry();
        let mut info = bplist::decode(bytes)?;
        info.as_dictionary_mut().ok_or(f50_core::Error::Malformed)?.insert(name, description);
        // LIVI's display-panel declaration, including the synthetic local-instrument panel.
        // No physical Tesla display or native instrument cluster is claimed by this probe.
        if let Some(display) = info.as_dictionary().and_then(|d| d.get("displays")).and_then(bplist::Value::as_array).and_then(|a| a.first()).cloned() {
            let d = display.as_dictionary().ok_or(f50_core::Error::Malformed)?;
            let uuid = d.get("uuid").and_then(bplist::Value::as_string).ok_or(f50_core::Error::Malformed)?;
            let panel = |uid: String, instruments: bool| {
                let mut p = bplist::dict([
                    ("uid", bplist::Value::String(uid)),
                    ("widthPixels", d["widthPixels"].clone()), ("heightPixels", d["heightPixels"].clone()),
                    ("widthPhysical", d["widthPhysical"].clone()), ("heightPhysical", d["heightPhysical"].clone()),
                    ("properties", bplist::Value::Array(if instruments { vec![bplist::Value::String("showsInstruments".into())] } else { vec![] })),
                    ("initialVideoStreams", bplist::Value::Array(if instruments { vec![] } else { vec![bplist::dict([
                        ("uuid", bplist::Value::String(uuid.into())), ("originXPixels", bplist::number(0)), ("originYPixels", bplist::number(0)), ("zIndex", bplist::number(0))
                    ])] })),
                ]);
                if instruments { p.as_dictionary_mut().unwrap().insert("displayPlugins".into(), bplist::Value::Array(vec![bplist::dict([
                    ("uid", bplist::Value::String("CowPlay-probe-cluster-plugin".into())), ("videoStreams", bplist::Value::Array(vec![]))
                ])])); }
                p
            };
            let panels = vec![panel(format!("{uuid}-panel"), false), panel("CowPlay-probe-cluster-panel".into(), true)];
            info.as_dictionary_mut().unwrap().insert("displayPanels".into(), bplist::Value::Array(panels));
        }
        bplist::encode(&info)
    }

    fn handle(&self, lane: u8, body: &[u8]) -> io::Result<()> {
        if body.len() > MAX_PACKAGE || livi::opack::decode(body).is_none() {
            return Err(failure("invalid Ultra OPACK message"));
        }
        let mut state=self.state.lock().unwrap();
        let answers = state.handle(if lane == 0 { Lane::High } else { Lane::Normal }, body);
        let senders = self.outgoing.lock().unwrap();
        for (target, message) in answers {
            let target = if target == Lane::High { 0 } else { 1 };
            let sender = senders.get(&target).or_else(|| senders.get(&lane))
                .ok_or_else(|| failure("Ultra reply channel missing"))?;
            sender.try_send(message).map_err(failure)?;
        }
        Ok(())
    }
    fn send(&self, lane: u8, message: Vec<u8>) -> io::Result<()> {
        self.outgoing.lock().unwrap().get(&lane).ok_or_else(||failure("Ultra reply channel missing"))?.try_send(message).map_err(failure)
    }
    fn start_native(&self, context: u64, grant: watch::Receiver<Option<u64>>, command: Option<ClimateCommand>, request: Option<(i64, Vec<u8>)>) -> Option<NativePending> {
        if self.native_busy.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).is_err() { return None; }
        match self.feed.request_climate(context,command,grant.clone()) {
            Some(reply) => Some(NativePending { request,reply,context,deadline:tokio::time::Instant::now()+Duration::from_secs(45),grant,busy:self.native_busy.clone() }),
            None => { self.native_busy.store(false,Ordering::Release); None }
        }
    }
    fn native_write(&self, lane: u8, body: &[u8]) -> io::Result<Option<Option<NativePending>>> {
        let grant = self.native.subscribe();
        let climate = self.feed.climate();
        let Some((transaction, command)) = VehicleState::live_climate_write(body,climate.current()) else { return Ok(None); };
        if *grant.borrow() == Some(climate.context) && !grant.has_changed().unwrap_or(true) {
            if let Some(id) = transaction {
                let cache = self.native_replies.lock().unwrap();
                if let Some((context,prior,response)) = cache.get(&id) {
                    self.send(lane,if *context==climate.context && prior==body { response.clone() } else { VehicleState::live_climate_response(body,false) })?;
                    return Ok(Some(None));
                }
                if let (Ok(command),true) = (command,cache.len() < 256) {
                    drop(cache);
                    if let Some(pending) = self.start_native(climate.context,grant,Some(command),Some((id,body.to_vec()))) {
                        self.native_replies.lock().unwrap().insert(id,(climate.context,body.to_vec(),VehicleState::live_climate_response(body,false)));
                        return Ok(Some(Some(pending)));
                    }
                }
            }
        }
        self.climate_notice(if climate.current().is_none() {
            if self.feed.snapshot().connected { "温控状态不可用，请读取车辆状态后重试" }
            else { "车辆未连接，操作未发送；当前为温控预设" }
        } else if *self.native.borrow() != Some(climate.context) {
            "温控未授权，请在车辆管理中允许本次会话"
        } else {
            "温控操作未执行，请检查温度范围或等待当前操作完成"
        });
        self.send(lane,VehicleState::live_climate_response(body,false))?;
        Ok(Some(None))
    }
    fn native_finished(&self, lane: u8, mut pending: NativePending, confirmed: bool) -> io::Result<()> {
        let confirmed = confirmed && *pending.grant.borrow() == Some(pending.context) && !pending.grant.has_changed().unwrap_or(true) && pending.context == self.feed.climate().context;
        if let Some((id,body)) = pending.request.take() {
            if !confirmed { self.climate_notice("车辆未确认本次温控操作，请检查连接后重试"); }
            let response = VehicleState::live_climate_response(&body,confirmed);
            self.native_replies.lock().unwrap().insert(id,(pending.context,body,response.clone()));
            self.send(lane,response)?;
        } else if !confirmed || self.feed.climate().current().is_none() { self.native.send_replace(None); }
        self.refresh()
    }
}

pub fn augment_info(bytes: Vec<u8>) -> Vec<u8> {
    if enabled() { Probe::new().augment_info(&bytes).expect("static Ultra description") } else { bytes }
}

fn package(kind: u32, message_type: u32, token: u64, body: &[u8]) -> io::Result<Vec<u8>> {
    if body.len() > MAX_PACKAGE - 32 { return Err(failure("Ultra package limit")); }
    let mut packet = vec![0; 32];
    packet[..4].copy_from_slice(&((body.len() + 32) as u32).to_be_bytes());
    packet[4..8].copy_from_slice(&kind.to_be_bytes());
    packet[16..20].copy_from_slice(&message_type.to_be_bytes());
    packet[20..28].copy_from_slice(&token.to_be_bytes());
    packet.extend_from_slice(body);
    Ok(packet)
}

pub async fn stream(listener: TcpListener, peer: IpAddr, shared: [u8;32], seed: u64, probe: Arc<Probe>, lane: u8) -> io::Result<()> {
    let salt = format!("DataStream-Salt{seed}");
    let read = derive(&shared, salt.as_bytes(), b"DataStream-Output-Encryption-Key").map_err(failure)?;
    let write = derive(&shared, salt.as_bytes(), b"DataStream-Input-Encryption-Key").map_err(failure)?;
    let (socket, address) = tokio::time::timeout(Duration::from_secs(30), listener.accept()).await??;
    if address.ip() != peer { return Err(failure("Ultra peer mismatch")); }
    socket.set_nodelay(true)?;
    let (tx, rx) = mpsc::channel(16);
    probe.outgoing.lock().unwrap().insert(lane, tx.clone());
    let _registration = Registration { probe: probe.clone(), lane, sender: tx };
    crate::diag::diag().event("ok", "ultra", format!("车辆状态通道 {lane} 已连接；有效值来自Tesla BLE"));
    duplex(socket, ControlCipher::new(read, write), probe.clone(), lane, rx).await
}

async fn duplex(socket: TcpStream, mut cipher: ControlCipher, probe: Arc<Probe>, lane: u8, mut outgoing: mpsc::Receiver<Vec<u8>>) -> io::Result<()> {
    let (mut rd, mut wr) = socket.into_split();
    let mut sealed = Vec::new();
    let mut plain = Vec::new();
    let mut read = [0;16384];
    let mut vehicle_updates=probe.feed.subscribe();
    let mut climate_updates=probe.feed.subscribe_climate();
    let mut refresh = tokio::time::interval(Duration::from_secs(5));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pending: Option<NativePending> = None;
    probe.refresh()?;
    loop {
        tokio::select! {
            updated=vehicle_updates.changed()=>{updated.map_err(failure)?;probe.refresh()?;}
            updated=climate_updates.changed()=>{updated.map_err(failure)?;probe.refresh()?;}
            _=refresh.tick()=>{
                probe.refresh()?;
                let climate=probe.feed.climate();
                if pending.is_none() && *probe.native.borrow() == Some(climate.context) && climate.received_ms.is_none_or(|at| feed::now_ms().saturating_sub(at)>=10_000) {
                    pending=probe.start_native(climate.context,probe.native.subscribe(),None,None);
                }
            }
            result=async { match pending.as_mut() { Some(p)=>tokio::time::timeout_at(p.deadline,&mut p.reply).await.unwrap_or(Ok(false)), None=>std::future::pending().await } }=>{
                probe.native_finished(lane,pending.take().unwrap(),result.unwrap_or(false))?;
            }
            received = rd.read(&mut read) => {
                let n = received?;
                if n == 0 { return Ok(()); }
                if sealed.len() + n > MAX_PACKAGE { return Err(failure("Ultra encrypted buffer limit")); }
                sealed.extend_from_slice(&read[..n]);
                let (decrypted, used) = cipher.decrypt(&sealed).map_err(failure)?;
                sealed.drain(..used);
                if plain.len() + decrypted.len() > MAX_PACKAGE { return Err(failure("Ultra plaintext buffer limit")); }
                plain.extend(decrypted);
                while plain.len() >= 32 {
                    let size = u32::from_be_bytes(plain[..4].try_into().unwrap()) as usize;
                    if !(32..=MAX_PACKAGE).contains(&size) { return Err(failure("Ultra package size")); }
                    if plain.len() < size { break; }
                    let kind = u32::from_be_bytes(plain[4..8].try_into().unwrap());
                    if kind == SYNC {
                        let token = u64::from_be_bytes(plain[20..28].try_into().unwrap());
                        let ack = cipher.encrypt(&package(REPLY, 0, token, &[])?).map_err(failure)?;
                        tokio::time::timeout(Duration::from_secs(3), wr.write_all(&ack)).await??;
                    }
                    if kind != REPLY {
                        match probe.native_write(lane,&plain[32..size])? {
                            Some(Some(request)) => pending=Some(request),
                            Some(None) => {},
                            None => probe.handle(lane,&plain[32..size])?,
                        }
                    }
                    plain.drain(..size);
                }
            }
            message = outgoing.recv() => {
                let Some(message) = message else { return Ok(()); };
                let bytes = cipher.encrypt(&package(ASYNC, COMMAND, 0, &message)?).map_err(failure)?;
                tokio::time::timeout(Duration::from_secs(3), wr.write_all(&bytes)).await??;
            }
        }
    }
}

#[cfg(test)]
mod tests;
