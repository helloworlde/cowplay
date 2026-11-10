//! Ultra live vehicle state. All numeric readings originate from the shared authenticated feed.
//! COWPLAY_ULTRA_PROBE=0 retains the existing runtime disable behavior.
mod livi;
pub use crate::vehicle as feed;

use f50_core::{bplist, crypto::{derive, ControlCipher}};
use livi::vehicle_state::{Lane, VehicleState};
use std::{collections::BTreeMap, io, net::IpAddr, sync::{Arc, Mutex}, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, sync::mpsc};

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
        }
    }
}

impl Probe {
    pub fn new() -> Arc<Self> {
        Self::with_feed(feed::Feed::new())
    }
    pub fn with_feed(feed:Arc<feed::Feed>)->Arc<Self>{
        let mut state=live_vehicle();state.set_live_values(&feed.snapshot().values);
        Arc::new(Self {state:Mutex::new(state),outgoing:Mutex::new(BTreeMap::new()),feed})
    }
    fn refresh(&self)->io::Result<()> {
        let mut state=self.state.lock().unwrap();let snapshot=self.feed.snapshot();let mut updates=state.set_live_values(&snapshot.values);
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
        let encoded = livi::bplist::encode(&description);
        let value = bplist::decode(&encoded)?;
        let mut info = bplist::decode(bytes)?;
        info.as_dictionary_mut().ok_or(f50_core::Error::Malformed)?.insert(name, value);
        info.as_dictionary_mut().unwrap().insert("name".into(), bplist::Value::String("CowPlay Tesla BLE".into()));
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
    probe.refresh()?;
    loop {
        tokio::select! {
            updated=vehicle_updates.changed()=>{updated.map_err(failure)?;probe.refresh()?;}
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
                        probe.handle(lane, &plain[32..size])?;
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
