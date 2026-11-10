//! Network half of the receiver. Caller supplies identity and explicit local interface.
//! Bluetooth bootstrap is separate; no BlueZ or Android dependency lives here.
use crate::{Control, Hub};
use f50_core::{
    bplist::{self, dict, number, Value},
    control::{Control as AuthControl, Decision},
    crypto::{derive, ControlCipher},
    media::{ScreenDecoder, ScreenPacketKind},
    mfi::Identity,
    rtsp::{response, Decoder},
};
use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, watch},
    task::JoinSet,
};
use zeroize::Zeroizing;
fn err(e: impl std::fmt::Debug) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}"))
}
fn bounded<T>(r: f50_core::Result<T>) -> io::Result<T> {
    r.map_err(err)
}
fn validate_streams(streams: &[Value], ultra: bool) -> io::Result<()> {
    if streams.is_empty() || streams.len() > if ultra { 7 } else { 5 } {
        return Err(err("stream count limit"));
    }
    let mut identities = std::collections::BTreeSet::new();
    for value in streams {
        let stream = value.as_dictionary().ok_or_else(|| err("stream must be dictionary"))?;
        let kind = stream.get("type").and_then(Value::as_unsigned_integer).ok_or_else(|| err("missing stream type"))?;
        let uuid = if kind == 130 { stream.get("clientTypeUUID").and_then(Value::as_string).unwrap_or("").to_ascii_uppercase() } else { String::new() };
        if !identities.insert((kind, uuid)) { return Err(err("duplicate stream identity")); }
    }
    Ok(())
}
// Temporary fixed-label observations: never expose arbitrary command strings or payloads.
fn command_category(command: &Value) -> &'static str {
    match command
        .as_dictionary()
        .and_then(|d| d.get("type"))
        .and_then(Value::as_string)
    {
        Some("noop") => "NOOP",
        Some("modesChanged") => "MODES_CHANGED",
        Some("requestUI") => "REQUEST_UI",
        Some("disableBluetooth") => "DISABLE_BLUETOOTH",
        Some("duckAudio") => "DUCK_AUDIO",
        Some("unduckAudio") => "UNDUCK_AUDIO",
        Some("iAPSendMessage") => "IAP_SEND_MESSAGE",
        Some("hidSetInputMode") => "HID_SET_INPUT_MODE",
        Some("flushAudio") => "FLUSH_AUDIO",
        Some("updateVocoderInfo") => "UPDATE_VOCODER_INFO",
        Some("setUpStreams") => "SET_UP_STREAMS",
        Some("tearDownStreams") => "TEAR_DOWN_STREAMS",
        _ => "OTHER",
    }
}
fn audio_streams_present(command: &Value) -> bool {
    let Some(d) = command.as_dictionary() else {
        return false;
    };
    [Some(d), d.get("params").and_then(Value::as_dictionary)]
        .into_iter()
        .flatten()
        .filter_map(|d| d.get("streams").and_then(Value::as_array))
        .flatten()
        .any(|s| {
            s.as_dictionary()
                .and_then(|d| d.get("type"))
                .and_then(Value::as_unsigned_integer)
                .is_some_and(|n| (100..=102).contains(&n))
        })
}
fn public_version(value: Option<&Value>) -> &str {
    value
        .and_then(Value::as_string)
        .filter(|s| {
            !s.is_empty() && s.len() <= 24 && s.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        })
        .unwrap_or("UNREPORTED")
}
fn request_category(request: &f50_core::rtsp::Message) -> &'static str {
    match (
        request.method.as_str(),
        request.path.split('?').next().unwrap_or(""),
    ) {
        (_, "/info") => "INFO_TARGET",
        ("GET_PARAMETER", _) => "GET_PARAMETER",
        ("SET_PARAMETER", _) => "SET_PARAMETER",
        ("FLUSH", _) => "FLUSH",
        ("POST", "/diag-info") => "DIAG_INFO",
        ("POST", "/getProperty") => "GET_PROPERTY",
        ("POST", "/setProperty") => "SET_PROPERTY",
        _ => "OTHER",
    }
}
pub struct Credentials {
    pub pairing_store: Option<std::path::PathBuf>,
    pub endpoint: Option<Arc<f50_core::wireless::Endpoint>>,
    pub pairing_id: Vec<u8>,
    pub signing_seed: Zeroizing<[u8; 32]>,
    pub mfi: Arc<dyn Identity>,
    /// Startup declaration (default display); each connection rebuilds /info from the Hub's
    /// current display spec so quality changes apply on the next phone connection.
    pub info: Vec<u8>,
    pub info_base: InfoBase,
    pub width: u16,
    pub height: u16,
}
pub type Pairings = Arc<crate::phones::Phones>;
#[derive(Clone, Debug)]
pub struct InfoBase {
    pub name: String,
    pub model: String,
    pub manufacturer: String,
    pub device_id: String,
    pub bluetooth_mac: String,
}
impl Credentials {
    /// /info for one connection using the display spec selected in the settings.
    pub fn info_for(&self, spec: &crate::DisplaySpec) -> Vec<u8> {
        let b = &self.info_base;
        let bytes = f50_core::info::build(&f50_core::info::DisplayInfo {
            name: &b.name,
            model: &b.model,
            manufacturer: &b.manufacturer,
            device_id: &b.device_id,
            bluetooth_mac: &b.bluetooth_mac,
            width: spec.width,
            height: spec.height,
            fps: spec.fps,
            width_mm: spec.width_mm,
            hevc: spec.hevc,
            microphone: spec.microphone,
            right_hand_drive: spec.right_hand_drive,
        })
        .unwrap_or_else(|_| self.info.clone());
        crate::ultra::augment_info(bytes)
    }
    pub fn default_spec(&self) -> crate::DisplaySpec {
        crate::DisplaySpec { width: self.width, height: self.height, fps: 30, width_mm: 180, hevc: false, microphone: false, right_hand_drive: false }
    }
}
pub struct Receiver {
    pub credentials: Arc<Credentials>,
    pub hub: Arc<Hub>,
    pub pairing_window: Duration,
}
impl Receiver {
    pub async fn run(
        self,
        listener: TcpListener,
        mut commands: mpsc::Receiver<Control>,
        mut stop: watch::Receiver<bool>,
    ) -> io::Result<()> {
        let initial = self.hub.display_spec().unwrap_or_else(|| self.credentials.default_spec());
        self.hub.set_dimensions(initial.width, initial.height).map_err(err)?;
        let discovery = if self.credentials.endpoint.is_some() {
            Some(crate::discovery::Discovery::start(
                &self.credentials,
                listener.local_addr()?,
            )?)
        } else {
            None
        };
        if self.credentials.endpoint.is_some() {
            self.hub
                .set_status("WAITING_IPHONE_CONTROL; BLUETOOTH_HANDOFF_AVAILABLE");
        }
        self.hub.set_pair_setup_length(self.pairing_window);
        // Production new pairing is opened only by a selected-phone iAP2 handoff.
        if self.credentials.endpoint.is_none() {self.hub.open_pair_setup();}
        let mut tasks = JoinSet::new();
        let mut active: Option<mpsc::Sender<Control>> = None;
        let mut busy_observations = 0u8;
        let mut active_epoch = None;
        loop {
            tokio::select! {
                result = crate::discovery::next(&discovery) => result?,
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                }
                Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                    let switching = active_epoch.is_some_and(|e| !self.hub.phones.is_current(e));
                    let ending = Ending::classify(&result, switching, self.hub.take_planned_restart());
                    report_ending(ending);
                    active = None;
                    while commands.try_recv().is_ok() {}
                    self.hub.reset();
                    self.hub.phone_active.store(false, std::sync::atomic::Ordering::Release);
                    active_epoch = None;
                    self.hub.set_status(ending.status());
                }
                accepted = listener.accept() => {
                    let (socket, addr) = accepted?;
                    let selection = self.hub.phones.snapshot();
                    if !selection.admission {
                        continue;
                    }
                    if active.is_some() {
                        if busy_observations < 16 {
                            busy_observations += 1;
                            crate::diag_echo!(Debug, "carplay", "control_connection=BUSY_REJECTED");
                        }
                        continue;
                    }
                    while commands.try_recv().is_ok() {}
                    let (tx, rx) = mpsc::channel(32);
                    active = Some(tx);
                    active_epoch = Some(selection.epoch);
                    *self.hub.clock.lock().unwrap() = crate::clock::Clock::default();
                    self.hub.phone_active.store(true, std::sync::atomic::Ordering::Release);
                    report_control_connected(addr.ip());
                    let (credentials, hub) = (self.credentials.clone(), self.hub.clone());
                    let window = self.hub.pair_setup_remaining();
                    tasks.spawn(async move {
                        let result = connection(socket, credentials, hub.clone(), rx, window, selection).await;
                        if let Err(error) = &result {
                            crate::diag_echo!(Warn, "carplay", "error kind={:?} reason={error}", error.kind());
                        }
                        hub.reset();
                        result
                    });
                }
                command = commands.recv() => {
                    if active_epoch.is_some_and(|e| !self.hub.phones.is_current(e)) {
                        continue;
                    }
                    match command {
                        Some(Control::EndSession) => {
                            if active.is_some() {
                                tasks.abort_all();
                                self.hub.set_status("DISPLAY_RECONFIGURED; RECONNECTING");
                            }
                        }
                        Some(c) => {
                            if active.as_ref().is_some_and(|tx| tx.try_send(c).is_err()) {
                                tasks.abort_all();
                                self.hub.set_status("CONTROL_BACKPRESSURE; RECONNECT_REQUIRED");
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        self.hub
            .phone_active
            .store(false, std::sync::atomic::Ordering::Release);
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        self.hub.reset();
        Ok(())
    }
}
/// Why a phone control session ended; selects the timeline reason, link step and status code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// The user switched or disconnected the phone.
    Switched,
    /// A settings change asked the phone to reconnect with a new declaration.
    Planned,
    Disconnected,
    Failed,
    Panicked,
}
impl Ending {
    fn classify(result: &Result<io::Result<()>, tokio::task::JoinError>, switching: bool, planned: bool) -> Self {
        match result {
            _ if switching => Ending::Switched,
            _ if planned => Ending::Planned,
            Ok(Ok(())) => Ending::Disconnected,
            Ok(Err(_)) => Ending::Failed,
            Err(_) => Ending::Panicked,
        }
    }
    fn reason(self) -> &'static str {
        match self {
            Ending::Switched => "用户切换或断开手机",
            Ending::Planned => "设置变更，重新连接 iPhone",
            Ending::Disconnected => "iPhone 正常断开",
            Ending::Failed => "协议或传输错误",
            Ending::Panicked => "会话任务异常",
        }
    }
    fn status(self) -> &'static str {
        match self {
            Ending::Switched => "PHONE_SELECTION_CHANGED; WAITING_FOR_PHONE",
            Ending::Planned => "SESSION_RESTARTING_FOR_SETTINGS",
            Ending::Disconnected => "IPHONE_DISCONNECTED; READY_FOR_RECONNECT",
            Ending::Failed => "IPHONE_PROTOCOL_OR_TRANSPORT_FAILED; READY_FOR_RECONNECT",
            Ending::Panicked => "NATIVE_SESSION_TASK_FAILED",
        }
    }
}
/// Close the diagnostic session and tell every page what happened and what to do next.
fn report_ending(ending: Ending) {
    let d = crate::diag::diag();
    let (reached_record, got_video) = d
        .sessions()
        .0
        .map(|c| {
            (
                c.events.iter().any(|e| e.msg.contains("CarPlay 会话已建立")),
                c.events.iter().any(|e| e.msg.contains("收到 iPhone 视频流")),
            )
        })
        .unwrap_or((false, false));
    d.session_end(ending.reason());
    let restarting = matches!(ending, Ending::Switched | Ending::Planned);
    if !restarting && reached_record && !got_video {
        d.link_fail("carplay", "CarPlay 会话在首帧前结束", "尝试“恢复兼容画质”后重新连接；仍失败请查看本次会话日志。");
        return;
    }
    match ending {
        Ending::Switched => {
            d.link_step("carplay", "idle", "旧手机已断开，等待选择的手机");
            d.link_step("wifi", "idle", "等待新的手机连接");
        }
        Ending::Planned => d.link_step("carplay", "active", "设置已变更，正在重新连接 iPhone…"),
        Ending::Disconnected => {
            d.link_step("carplay", "idle", "iPhone 已断开，点“连接 iPhone”可重新连接");
            d.link_step("wifi", "idle", "iPhone 已离开会话");
        }
        Ending::Failed | Ending::Panicked => {
            if d.link().carplay != "fail" {
                d.link_fail("carplay", "CarPlay 会话因协议或传输错误中断", "点“连接 iPhone”重试；反复出现请在诊断页导出诊断包。");
            }
        }
    }
}
fn report_control_connected(ip: IpAddr) {
    let d = crate::diag::diag();
    d.session_begin("control");
    d.set_peer(ip.to_string(), None);
    d.event("ok", "carplay", format!("iPhone 控制连接 {ip}"));
    d.link_step("wifi", "ok", format!("iPhone 已通过 Wi‑Fi 连上 F50（{ip}）"));
    d.link_step("carplay", "active", "控制连接已建立，开始鉴权");
}
async fn write(socket: &mut TcpStream, bytes: &[u8]) -> io::Result<()> {
    tokio::time::timeout(Duration::from_secs(3), socket.write_all(bytes))
        .await
        .map_err(err)??;
    Ok(())
}
async fn read_control(
    socket: &mut TcpStream,
    bytes: &mut [u8],
    activity: &mut watch::Receiver<tokio::time::Instant>,
) -> io::Result<usize> {
    let budget = Duration::from_secs(60);
    let mut deadline = tokio::time::Instant::now() + budget;
    loop {
        tokio::select! {
            biased;
            read = tokio::time::timeout_at(deadline, socket.read(bytes)) => match read {
                Ok(result) => return result,
                Err(expired) => {
                    // A beacon and the timer can both be ready after delayed scheduling.
                    let renewed = *activity.borrow_and_update() + budget;
                    if renewed <= deadline { return Err(err(expired)); }
                    deadline = renewed;
                }
            },
            changed = activity.changed() => {
                changed.map_err(err)?;
                deadline = deadline.max(*activity.borrow_and_update() + budget);
            },
        }
    }
}
fn control_features(hevc: bool, iap_channel: bool, ultra: bool, phone: &bplist::Dictionary) -> Vec<Value> {
    let mut features = vec![Value::String("viewAreas".into())];
    if hevc { features.push(Value::String("hevc".into())); }
    if iap_channel { features.push(Value::String("iAPChannel".into())); }
    if ultra && phone.get("features").and_then(Value::as_array)
        .is_some_and(|offered| offered.iter().any(|v| v.as_string() == Some(crate::ultra::FEATURE))) {
        features.push(Value::String(crate::ultra::FEATURE.into()));
    }
    features
}

async fn control_setup_response(
    mut reply: Value,
    keepalive_requested: bool,
    local: IpAddr,
    peer: IpAddr,
    tasks: &mut JoinSet<io::Result<()>>,
    activity: watch::Sender<tokio::time::Instant>,
) -> io::Result<Vec<u8>> {
    let keepalive = if keepalive_requested {
        Some(UdpSocket::bind(SocketAddr::new(local, 0)).await?)
    } else {
        None
    };
    if let Some(socket) = &keepalive {
        reply
            .as_dictionary_mut()
            .ok_or_else(|| err("control SETUP reply must be a dictionary"))?
            .insert(
                "keepAlivePort".into(),
                number(socket.local_addr()?.port() as u64),
            );
    }
    let encoded = bounded(bplist::encode(&reply))?;
    if let Some(socket) = keepalive {
        tasks.spawn(keepalive_stream(socket, peer, activity));
        crate::diag_echo!(Debug, "carplay", "keepalive_transport=BOUND");
    }
    Ok(encoded)
}
async fn keepalive_stream(
    socket: UdpSocket,
    peer: IpAddr,
    activity: watch::Sender<tokio::time::Instant>,
) -> io::Result<()> {
    // Beacons are receive-only. Fixed storage also detects oversized truncated datagrams.
    let mut buffer = [0; 9];
    let mut observed = false;
    loop {
        let (n, source) = socket.recv_from(&mut buffer).await?;
        if source.ip() != peer || n == 0 || n > 8 || buffer[0] >> 6 != 0 {
            continue;
        }
        activity.send_replace(tokio::time::Instant::now());
        if !observed {
            observed = true;
            crate::diag_echo!(Debug, "carplay",
                "keepalive_beacon=RECEIVED sleep={}",
                buffer[0] & 0x20 != 0
            );
        }
    }
}
async fn accept_peer(listener: TcpListener, peer: IpAddr) -> io::Result<TcpStream> {
    tokio::time::timeout(Duration::from_secs(30), async move {
        for _ in 0..8 {
            let (s, a) = listener.accept().await?;
            if a.ip() == peer {
                return Ok(s);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "media peer mismatch",
        ))
    })
    .await
    .map_err(err)?
}
/// Plain-language reason and next action for a refused CarPlay control step.
fn auth_failure(stage: &str, error: &str) -> (String, String) {
    match (stage, error) {
        ("PAIR_SETUP", "State") => (
            "iPhone 请求 CarPlay 配对，但 F50 的配对窗口已关闭".into(),
            "点“连接 iPhone”（蓝牙认证后会自动重新开放 2 分钟）；仍失败请重启服务。".into(),
        ),
        ("PAIR_VERIFY", _) => (
            "CarPlay 配对验证失败：iPhone 保存的配对与 F50 不一致".into(),
            "在 iPhone「设置 › 通用 › CarPlay」中忽略此车，再从手机连接页重新配对。".into(),
        ),
        ("AUTH_SETUP", _) => (
            "配件身份认证失败".into(),
            "检查配件身份文件（自检“配件身份”）；导出诊断包以便排查。".into(),
        ),
        _ => (
            format!("CarPlay 鉴权在 {stage} 步骤失败（{error}）"),
            "点“连接 iPhone”重试；反复出现请导出诊断包。".into(),
        ),
    }
}
async fn connection(
    mut socket: TcpStream,
    credentials: Arc<Credentials>,
    hub: Arc<Hub>,
    commands: mpsc::Receiver<Control>,
    pair_window: Duration,
    selection: crate::phones::Snapshot,
) -> io::Result<()> {
    let local = socket.local_addr()?.ip();
    let peer = socket.peer_addr()?.ip();
    socket.set_nodelay(true)?;
    let started = Instant::now();
    let spec = hub.display_spec().unwrap_or_else(|| credentials.default_spec());
    hub.set_dimensions(spec.width, spec.height).map_err(err)?;
    crate::diag::diag().event("info", "carplay", format!("声明画面 {}×{}@{} · {} · 宽 {} mm", spec.width, spec.height, spec.fps, if spec.hevc { "H.265 可选" } else { "H.264" }, spec.width_mm));
    let mut auth = bounded(AuthControl::new(
        credentials.pairing_id.clone(),
        *credentials.signing_seed,
        credentials.info_for(&spec),
        pair_window.as_millis() as u64,
    ))?;
    let mut decoder = Decoder::default();
    let mut cipher: Option<ControlCipher> = None;
    let mut shared: Option<Zeroizing<[u8; 32]>> = None;
    let mut encrypted = Vec::new();
    let mut read = [0u8; 16384];
    let mut tasks: JoinSet<io::Result<()>> = JoinSet::new();
    let (event_tx, event_rx) = mpsc::channel(32);
    let tunnel_commands = event_tx.clone();
    tasks.spawn(async move {
        let mut commands = commands;
        while let Some(command) = commands.recv().await {
            event_tx.send(command).await.map_err(err)?;
        }
        Ok(())
    });
    let mut command_rx = Some(event_rx);
    let mut screen: Option<tokio::task::AbortHandle> = None;
    let mut basic_setup = false;
    let mut command_observations = 0u8;
    let mut command_categories = std::collections::BTreeSet::new();
    let mut response_categories = std::collections::BTreeSet::new();
    let mut info_observations = 0u8;
    let mut tunnel_task: Option<tokio::task::AbortHandle> = None;
    let probe = crate::ultra::enabled().then(||crate::ultra::Probe::with_feed(hub.vehicle.clone()));
    let mut probe_tasks = std::collections::BTreeMap::<u8, tokio::task::AbortHandle>::new();
    if probe.is_some() {
        crate::diag::diag().event("info", "ultra", "Ultra只读车辆数据；缺失或过期读数不可用");
    }
    let mut audio_tasks = std::collections::BTreeMap::<u8, tokio::task::AbortHandle>::new();
    let (keepalive_activity, mut keepalive_activity_rx) =
        watch::channel(tokio::time::Instant::now());
    hub.set_status("IPHONE_CONTROL_CONNECTED; AUTHENTICATION_PENDING");
    let result=tokio::select! {biased;
        _=hub.phones.changed_since(selection.epoch)=>Ok(()),
        result=async {loop {
        let n = tokio::select! {
         result=read_control(&mut socket,&mut read,&mut keepalive_activity_rx)=>result?,
         result=tasks.join_next(),if !tasks.is_empty()=>{match result{Some(Ok(Ok(())))=>return Ok(()),Some(Ok(Err(e)))=>return Err(e),Some(Err(e)) if e.is_cancelled()=>continue,Some(Err(_))=>return Err(err("native media task panicked")),None=>continue}}
        };
        if n == 0 {
            return Ok(());
        }
        if let Some(c) = cipher.as_mut() {
            if encrypted.len() + n > 1024 * 1024 {
                return Err(err("control buffer limit"));
            }
            encrypted.extend(&read[..n]);
            let (plain, used) = bounded(
                c.decrypt(&encrypted)
                    .inspect_err(|e| crate::diag_echo!(Warn, "carplay", "CONTROL_DECRYPT_ERROR {e:?}")),
            )?;
            encrypted.drain(..used);
            bounded(decoder.feed(&plain))?;
        } else {
            bounded(decoder.feed(&read[..n]))?;
        }
        while let Some(request) =
            bounded(decoder.next_message().inspect_err(|e| {
                crate::diag_echo!(Warn, "carplay", "CONTROL_FRAMING_ERROR {e:?} {:?}", decoder.rejection())
            }))?
        {
            // Only fixed stage names and a public step number are observable, never headers/body/IDs.
            let stage = match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/pair-setup") => "PAIR_SETUP",
                ("POST", "/pair-verify") => "PAIR_VERIFY",
                ("POST", "/auth-setup") => "AUTH_SETUP",
                (_, "/info") => "INFO",
                ("SETUP", _) => "SETUP",
                ("RECORD", _) => "RECORD",
                _ => "OTHER",
            };
            if matches!(
                stage,
                "PAIR_SETUP" | "PAIR_VERIFY" | "AUTH_SETUP" | "INFO" | "SETUP" | "RECORD"
            ) {
                let step = if matches!(stage, "PAIR_SETUP" | "PAIR_VERIFY") {
                    f50_core::framing::tlv8_decode(&request.body)
                        .ok()
                        .and_then(|t| t.get(&6).and_then(|v| v.first()).copied())
                } else {
                    None
                };
                crate::diag_echo!(Info, "carplay", "CONTROL_STAGE {stage} step={step:?}");
                let detail = match stage {
                    "PAIR_SETUP" => Some(("active", "CarPlay 配对建立中")),
                    "PAIR_VERIFY" => Some(("active", "CarPlay 配对验证中")),
                    "AUTH_SETUP" => Some(("active", "配件身份认证中")),
                    "SETUP" => Some(("active", "建立媒体会话")),
                    "RECORD" => Some(("ok", "已鉴权，正在传输画面")),
                    _ => None,
                };
                // Later SETUPs inside a live session must not downgrade a working step.
                if let Some((state, text)) = detail {
                    let d = crate::diag::diag();
                    if !(state == "active" && d.link().carplay == "ok") {
                        d.link_step("carplay", state, text);
                    }
                }
            }
            if request_category(&request) == "INFO_TARGET" && info_observations < 8 {
                info_observations += 1;
                let value = bplist::decode(&request.body).ok();
                let parse = if request.body.is_empty() {
                    "EMPTY"
                } else if value.is_some() {
                    "VALID"
                } else {
                    "INVALID"
                };
                let qualifiers = value
                    .as_ref()
                    .and_then(Value::as_dictionary)
                    .and_then(|d| d.get("qualifier"))
                    .and_then(Value::as_array);
                let has = |name| {
                    qualifiers.is_some_and(|q| q.iter().any(|v| v.as_string() == Some(name)))
                };
                let query = request.path.split_once('?').map(|(_, q)| q);
                let query_txt = query.is_some_and(|q| {
                    q.split('&')
                        .any(|v| v.split('=').next() == Some("txtAirPlay"))
                });
                crate::diag_echo!(Debug, "carplay", "info plist={parse} query={} query_txt={query_txt} qualifier_array={} body_txt={} audio_formats={} audio_latencies={} limit_reached={}",query.is_some(),qualifiers.is_some(),has("txtAirPlay"),has("audioFormats"),has("audioLatencies"),info_observations == 8);
            }
            let decision = bounded(
                auth.handle(
                    request,
                    credentials.mfi.as_ref(),
                    |id| hub.phones.lookup(&selection, id),
                    started.elapsed().as_millis() as u64,
                )
                .inspect_err(|e| {
                    crate::diag_echo!(Warn, "carplay", "CONTROL_AUTH_ERROR {stage} {e:?}");
                    let (title, hint) = auth_failure(stage, &format!("{e:?}"));
                    crate::diag::diag().link_fail("carplay", title, hint);
                }),
            )?;
            match decision {
                Decision::Reply {
                    wire,
                    enable,
                    paired,
                } => {
                    let wire = if let Some(c) = cipher.as_mut() {
                        bounded(c.encrypt(&wire))?
                    } else {
                        wire
                    };
                    if let Some(p) = paired {
                        hub.phones.save(&selection, &p.controller_id, &p.controller_key)?;
                    }
                    write(&mut socket, &wire).await?;
                    if let Some(v) = enable {
                        if !decoder.is_empty() {
                            return Err(err("plaintext pipelined across encryption boundary"));
                        }
                        cipher = Some(ControlCipher::new(*v.read_key, *v.write_key));
                        shared = Some(v.shared);
                        hub.phone_verified.store(true, std::sync::atomic::Ordering::Release);
                        hub.set_status("CONTROL_VERIFIED; ACCESSORY_PROOF_PENDING");
                    }
                }
                Decision::Authenticated(request) => {
                    let secret = shared
                        .as_ref()
                        .ok_or_else(|| err("missing verified secret"))?;
                    let mut status = 200;
                    let mut result = Vec::new();
                    match request.method.as_str() {
                        "SETUP" => {
                            let root = bounded(bplist::decode(&request.body))?;
                            let data = root
                                .as_dictionary()
                                .ok_or_else(|| err("SETUP must be dictionary"))?;
                            if let Some(streams) = data.get("streams") {
                                if !basic_setup {
                                    return Err(err("stream setup before event/timing setup"));
                                }
                                let streams = streams
                                    .as_array()
                                    .ok_or_else(|| err("streams must be array"))?;
                                if validate_streams(streams, probe.is_some()).is_err() {
                                    status = 461;
                                } else {
                                    let mut configured = Vec::new();
                                    for requested in streams {
                                        result.clear();
                                        let stream = requested
                                            .as_dictionary()
                                            .ok_or_else(|| err("stream must be dictionary"))?;
                                        let kind = stream
                                            .get("type")
                                            .and_then(Value::as_unsigned_integer)
                                            .ok_or_else(|| err("missing stream type"))?;
                                        // Public format/capability metadata only; no IDs, seeds, headers or body.
                                        crate::diag_echo!(Debug, "carplay", "setup type={kind} audio_format={:?} seed_present={} connection_id_present={} seed_signed_negative={} connection_id_signed_negative={}",stream.get("audioFormat").and_then(Value::as_unsigned_integer),stream.contains_key("seed"),stream.contains_key("streamConnectionID"),stream.get("seed").and_then(Value::as_signed_integer).is_some_and(|n|n<0),stream.get("streamConnectionID").and_then(Value::as_signed_integer).is_some_and(|n|n<0));
                                        if (100..=102).contains(&kind) {
                                            let format =
                                                bounded(f50_core::audio::Format::from_bits(
                                                    stream
                                                        .get("audioFormat")
                                                        .and_then(Value::as_unsigned_integer)
                                                        .ok_or_else(|| {
                                                            err("audio format missing")
                                                        })?,
                                                ))?;
                                            let name = stream
                                                .get("audioType")
                                                .and_then(Value::as_string)
                                                .unwrap_or("default");
                                            let voice = spec.microphone && kind == 100 && ["telephony", "speechRecognition"].contains(&name);
                                            let phone_port = stream.get("dataPort").and_then(Value::as_unsigned_integer).unwrap_or(0);
                                            if !(voice || ["compatibility", "default", "alert", "media"].contains(&name))
                                                || (phone_port != 0 && !voice)
                                                || phone_port > 65535
                                            {
                                                status = 461;
                                            } else {
                                                let id = stream
                                                    .get("streamConnectionID")
                                                    .and_then(bplist::stream_identifier)
                                                    .ok_or_else(|| {
                                                        err("audio connection ID missing")
                                                    })?;
                                                let key = bounded(derive(
                                                    &**secret,
                                                    format!("DataStream-Salt{id}").as_bytes(),
                                                    b"DataStream-Output-Encryption-Key",
                                                ))?;
                                                let data =
                                                    UdpSocket::bind(SocketAddr::new(local, 0))
                                                        .await?;
                                                let control =
                                                    UdpSocket::bind(SocketAddr::new(local, 0))
                                                        .await?;
                                                let data_port = data.local_addr()?.port();
                                                let control_port = control.local_addr()?.port();
                                                if voice && phone_port != 0 {
                                                    let input = bounded(derive(
                                                        &**secret,
                                                        format!("DataStream-Salt{id}").as_bytes(),
                                                        b"DataStream-Input-Encryption-Key",
                                                    ))?;
                                                    hub.mic_start(crate::MicTarget {
                                                        kind: kind as u8,
                                                        audio_type: name.to_owned(),
                                                        addr: SocketAddr::new(peer, phone_port as u16),
                                                        key: input,
                                                        rate: format.rate,
                                                        channels: format.channels,
                                                    });
                                                }
                                                hub.audio_metadata(kind as u8, format.rate, id);
                                                hub.audio_config(
                                                    kind as u8,
                                                    bounded(format.config(kind as u8, name))?,
                                                )
                                                .map_err(err)?;
                                                if let Some(old) = audio_tasks.remove(&(kind as u8))
                                                {
                                                    old.abort()
                                                }
                                                audio_tasks.insert(
                                                    kind as u8,
                                                    tasks.spawn(audio_stream(
                                                        data,
                                                        control,
                                                        peer,
                                                        key,
                                                        format,
                                                        kind as u8,
                                                        hub.clone(),
                                                    )),
                                                );
                                                result = bounded(bplist::encode(&dict([(
                                                    "streams",
                                                    Value::Array(vec![dict([
                                                        ("type", number(kind)),
                                                        ("dataPort", number(data_port as u64)),
                                                        (
                                                            "controlPort",
                                                            number(control_port as u64),
                                                        ),
                                                        ("streamConnectionID", number(id)),
                                                    ])]),
                                                )])))?;
                                            }
                                        } else if kind == 130 && probe.is_some() && stream.get("clientTypeUUID").and_then(Value::as_string).and_then(crate::ultra::Probe::lane).is_some() {
                                            let lane = stream.get("clientTypeUUID").and_then(Value::as_string).and_then(crate::ultra::Probe::lane).unwrap();
                                            let seed = stream.get("seed").and_then(bplist::stream_identifier).ok_or_else(|| err("Ultra tunnel seed missing"))?;
                                            let listener = TcpListener::bind(SocketAddr::new(local, 0)).await?;
                                            let port = listener.local_addr()?.port();
                                            if let Some(previous) = probe_tasks.remove(&lane) { previous.abort(); }
                                            let probe = probe.as_ref().unwrap().clone();
                                            let shared = **secret;
                                            let handle = tasks.spawn(crate::ultra::stream(listener, peer, shared, seed, probe, lane));
                                            probe_tasks.insert(lane, handle);
                                            result = bounded(bplist::encode(&dict([("streams", Value::Array(vec![dict([
                                                ("type", number(130)), ("streamID", number(0x100 + lane as u64)), ("dataPort", number(port as u64)),
                                            ])]))])))?;
                                        } else if kind == 130 && credentials.endpoint.is_some() {
                                            if stream
                                                .get("clientTypeUUID")
                                                .and_then(Value::as_string)
                                                .is_none_or(|s| {
                                                    !s.eq_ignore_ascii_case(
                                                        "E9459FD0-BCAD-4C45-820F-1E72447EF2F2",
                                                    )
                                                })
                                            {
                                                status = 461;
                                            } else {
                                                let seed = stream
                                                    .get("seed")
                                                    .and_then(bplist::stream_identifier)
                                                    .ok_or_else(|| err("tunnel seed missing"))?;
                                                let key = bounded(derive(
                                                    &**secret,
                                                    format!("DataStream-Salt{seed}").as_bytes(),
                                                    b"DataStream-Output-Encryption-Key",
                                                ))?;
                                                let listener =
                                                    TcpListener::bind(SocketAddr::new(local, 0))
                                                        .await?;
                                                let port = listener.local_addr()?.port();
                                                if let Some(previous) = tunnel_task.take() {
                                                    previous.abort()
                                                }
                                                let tunnel_credentials = credentials.clone();
                                                let tunnel_tx = tunnel_commands.clone();
                                                let tunnel_vehicle=hub.vehicle.clone();
                                                tunnel_task = Some(tasks.spawn(async move {
                                                    iap_tunnel(
                                                        listener,
                                                        peer,
                                                        key,
                                                        tunnel_credentials,
                                                        tunnel_tx,
                                                        tunnel_vehicle,
                                                    )
                                                    .await
                                                    .inspect_err(|_| {
                                                        crate::diag_echo!(Debug, "carplay",
                                                            "source=IAP_TUNNEL"
                                                        )
                                                    })
                                                }));
                                                result = bounded(bplist::encode(&dict([(
                                                    "streams",
                                                    Value::Array(vec![dict([
                                                        ("type", number(130)),
                                                        ("streamID", number(1)),
                                                        ("dataPort", number(port as u64)),
                                                    ])]),
                                                )])))?;
                                            }
                                        } else if kind != 110 {
                                            status = 461;
                                            hub.set_status(
                                            "UNSUPPORTED_MEDIA_STREAM; IAP_TUNNEL_NOT_CONNECTED",
                                        );
                                        } else {
                                            let id = stream
                                                .get("streamConnectionID")
                                                .and_then(bplist::stream_identifier)
                                                .ok_or_else(|| {
                                                    err("missing stream connection ID")
                                                })?;
                                            let key = bounded(derive(
                                                &**secret,
                                                format!("DataStream-Salt{id}").as_bytes(),
                                                b"DataStream-Output-Encryption-Key",
                                            ))?;
                                            let listener =
                                                TcpListener::bind(SocketAddr::new(local, 0))
                                                    .await?;
                                            let port = listener.local_addr()?.port();
                                            if let Some(previous) = screen.take() {
                                                previous.abort()
                                            }
                                            let screen_hub = hub.clone();
                                            let (w, h) = (spec.width, spec.height);
                                            screen = Some(tasks.spawn(async move {
                                                screen_stream(listener, peer, key, screen_hub, w, h)
                                                    .await
                                                    .inspect_err(|_| {
                                                        crate::diag_echo!(Debug, "carplay",
                                                            "source=SCREEN"
                                                        )
                                                    })
                                            }));
                                            let entry = dict([
                                                ("type", number(110)),
                                                ("dataPort", number(port as u64)),
                                            ]);
                                            result = bounded(bplist::encode(&dict([(
                                                "streams",
                                                Value::Array(vec![entry]),
                                            )])))?;
                                        }
                                        if status != 200 {
                                            break;
                                        }
                                        let response = bounded(bplist::decode(&result))?;
                                        let entries = response
                                            .as_dictionary()
                                            .and_then(|d| d.get("streams"))
                                            .and_then(Value::as_array)
                                            .ok_or_else(|| err("stream response missing"))?;
                                        configured.extend(entries.iter().cloned());
                                    }
                                    result = if status == 200 {
                                        bounded(bplist::encode(&dict([(
                                            "streams",
                                            Value::Array(configured),
                                        )])))?
                                    } else {
                                        Vec::new()
                                    };
                                }
                            } else if basic_setup {
                                status = 409;
                            } else {
                                let model = data
                                    .get("model")
                                    .and_then(Value::as_string)
                                    .filter(|s| {
                                        s.starts_with("iPhone")
                                            && s.len() <= 24
                                            && !s[6..].is_empty()
                                            && s[6..]
                                                .bytes()
                                                .all(|b| b.is_ascii_digit() || b == b',')
                                    })
                                    .unwrap_or("UNREPORTED");
                                let flag = |key| {
                                    data.get(key).and_then(Value::as_boolean).or_else(|| {
                                        data.get(key)
                                            .and_then(Value::as_unsigned_integer)
                                            .filter(|n| *n <= 1)
                                            .map(|n| n != 0)
                                    })
                                };
                                crate::diag::diag().set_phone_info((model != "UNREPORTED").then(|| model.to_owned()), Some(public_version(data.get("osVersion"))).filter(|v| *v != "UNREPORTED").map(str::to_owned));
                                crate::diag_echo!(Debug, "carplay", "initial_setup model={model} os_version={} source_version={} screen_mirroring={:?} keep_alive_low_power={:?} update_session={:?}",public_version(data.get("osVersion")),public_version(data.get("sourceVersion")),flag("isScreenMirroringSession"),flag("keepAliveLowPower"),flag("updateSessionRequest"));
                                let listener = TcpListener::bind(SocketAddr::new(local, 0)).await?;
                                let event_port = listener.local_addr()?.port();
                                let timing = UdpSocket::bind(SocketAddr::new(local, 0)).await?;
                                let timing_port = timing.local_addr()?.port();
                                let phone_timing = data
                                    .get("timingPort")
                                    .and_then(Value::as_unsigned_integer)
                                    .unwrap_or(0);
                                if phone_timing > 65535 {
                                    return Err(err("invalid timing port"));
                                }
                                let read_key = bounded(derive(
                                    &**secret,
                                    b"Events-Salt",
                                    b"Events-Read-Encryption-Key",
                                ))?;
                                let write_key = bounded(derive(
                                    &**secret,
                                    b"Events-Salt",
                                    b"Events-Write-Encryption-Key",
                                ))?;
                                let rx = command_rx
                                    .take()
                                    .ok_or_else(|| err("event channel already owned"))?;
                                let night = hub.night_mode();
                                tasks.spawn(async move {
                                    event_stream(
                                        listener,
                                        peer,
                                        ControlCipher::new(read_key, write_key),
                                        rx,
                                        night,
                                    )
                                    .await
                                });
                                let timing_hub = hub.clone();
                                tasks.spawn(async move {
                                    timing_loop(
                                        timing,
                                        SocketAddr::new(peer, phone_timing as u16),
                                        timing_hub,
                                    )
                                    .await
                                });
                                basic_setup = true;
                                let enabled_features = control_features(spec.hevc, credentials.endpoint.is_some(), probe.is_some(), data);
                                if probe.is_some() {
                                    let offered = enabled_features.iter().any(|v| v.as_string() == Some(crate::ultra::FEATURE));
                                    crate::diag::diag().event("info", "ultra", format!("iPhone vehicleStateProtocol={offered}"));
                                }
                                result = control_setup_response(
                                    dict([
                                        ("timingPort", number(timing_port as u64)),
                                        ("eventPort", number(event_port as u64)),
                                        (
                                            "enabledFeatures",
                                            Value::Array(enabled_features),
                                        ),
                                    ]),
                                    flag("keepAliveLowPower") == Some(true),
                                    local,
                                    peer,
                                    &mut tasks,
                                    keepalive_activity.clone(),
                                )
                                .await?;
                            }
                        }
                        "RECORD" => {
                            if !basic_setup {
                                status = 409;
                            } else {
                                hub.set_status("IPHONE_RECORD_RECEIVED; WAITING_FOR_VIDEO");
                            }
                        }
                        "TEARDOWN" => {
                            let kinds = if request.body.is_empty() {
                                vec![(100, None), (101, None), (102, None), (110, None), (130, None)]
                            } else {
                                let value = bounded(bplist::decode(&request.body))?;
                                // A body without "streams" is the iPhone ending the whole session.
                                let empty = Vec::new();
                                let entries = match value.as_dictionary().and_then(|d| d.get("streams")) {
                                    Some(v) => v.as_array().ok_or_else(|| err("teardown streams malformed"))?,
                                    None => {
                                        let keys: Vec<String> = value.as_dictionary().map(|d| d.keys().cloned().collect()).unwrap_or_default();
                                        crate::diag_echo!(Info, "carplay", "TEARDOWN_ALL keys={keys:?}");
                                        &empty
                                    }
                                };
                                if entries.len() > if probe.is_some() { 7 } else { 5 } {
                                    return Err(err("teardown stream limit"));
                                }
                                if entries.is_empty() {
                                    vec![(100, None), (101, None), (102, None), (110, None), (130, None)]
                                } else {
                                    entries
                                        .iter()
                                        .map(|v| {
                                            let fields = v.as_dictionary().ok_or_else(|| err("teardown stream malformed"))?;
                                            let kind = fields.get("type").and_then(Value::as_unsigned_integer).ok_or_else(|| err("teardown type missing"))?;
                                            let id = fields.get("streamID").and_then(bplist::stream_identifier);
                                            Ok((kind, id))
                                        })
                                        .collect::<io::Result<Vec<_>>>()?
                                }
                            };
                            for (kind, stream_id) in kinds {
                                match kind {
                                    100..=102 => {
                                        if let Some(task) = audio_tasks.remove(&(kind as u8)) {
                                            task.abort()
                                        }
                                        hub.stop_audio(kind as u8);
                                    }
                                    110 => {
                                        if let Some(task) = screen.take() {
                                            task.abort()
                                        }
                                        hub.clear_video();
                                    }
                                    130 => {
                                        match stream_id {
                                            Some(id @ 0x100..=0x101) => {
                                                if let Some(task) = probe_tasks.remove(&((id - 0x100) as u8)) { task.abort(); }
                                            }
                                            Some(1) => { if let Some(task) = tunnel_task.take() { task.abort(); } }
                                            Some(_) => status = 461,
                                            None => {
                                                if let Some(task) = tunnel_task.take() { task.abort(); }
                                                for (_, task) in std::mem::take(&mut probe_tasks) { task.abort(); }
                                            }
                                        }
                                    }
                                    _ => status = 461,
                                }
                            }
                            hub.set_status("REQUESTED_STREAMS_STOPPED; CONTROL_STILL_CONNECTED");
                        }
                        "GET_PARAMETER" | "SET_PARAMETER" => {}
                        "POST" if request.path == "/feedback" => {
                            result = bounded(bplist::encode(&dict([(
                                "streams",
                                Value::Array(hub.feedback()),
                            )])))?;
                        }
                        "POST" if request.path == "/command" => {
                            let command = bounded(bplist::decode(&request.body))?;
                            let category = command_category(&command);
                            let audio_streams = audio_streams_present(&command);
                            if command_categories.insert((category, audio_streams)) {
                                crate::diag_echo!(Debug, "audio", "channel=CONTROL command={category} audio_streams={audio_streams}");
                            }
                            if category == "REQUEST_UI" {
                                hub.request_host_ui();
                                crate::diag::diag().event("info", "carplay", "在 CarPlay 主屏点了 CowPlay：打开车机设置");
                            }
                            if category == "MODES_CHANGED" && command_observations < 16 {
                                command_observations += 1;
                                let dictionary = command.as_dictionary();
                                if category == "MODES_CHANGED" {
                                    let params = dictionary
                                        .and_then(|d| d.get("params"))
                                        .and_then(Value::as_dictionary);
                                    let resources = params
                                        .and_then(|d| {
                                            d.get("resources").or_else(|| {
                                                d.get("modes")
                                                    .and_then(Value::as_dictionary)
                                                    .and_then(|m| m.get("resources"))
                                            })
                                        })
                                        .and_then(Value::as_array);
                                    for resource in resources.into_iter().flatten().take(2) {
                                        if let Some(d) = resource.as_dictionary() {
                                            let number = |key| {
                                                d.get(key)
                                                    .and_then(Value::as_unsigned_integer)
                                                    .filter(|n| *n <= 16)
                                            };
                                            if let Some(id @ (1 | 2)) = number("resourceID") {
                                                crate::diag_echo!(Debug, "audio", "resource={id} entity={:?} owner={:?} borrower={:?}",number("entity"),number("owner"),number("borrower"));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => status = 404,
                    }
                    if status != 200
                        || matches!(request.method.as_str(), "GET_PARAMETER" | "SET_PARAMETER")
                    {
                        let category = request_category(&request);
                        if response_categories.insert((category, status)) {
                            crate::diag_echo!(Debug, "carplay", "request={category} response={status}");
                        }
                    }
                    let wire = bounded(response(
                        &request,
                        status,
                        "application/x-apple-binary-plist",
                        &result,
                    ))?;
                    let encrypted = bounded(
                        cipher
                            .as_mut()
                            .ok_or_else(|| err("cipher missing"))?
                            .encrypt(&wire),
                    )?;
                    write(&mut socket, &encrypted).await?;
                    if request.method == "SETUP" && status != 200 {
                        return Err(err("stream negotiation rejected; session closed"));
                    }
                }
            }
        }
    }}=>result,
    };
    // Switching completes only after every old media socket/task has released ownership.
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    result
}
async fn iap_tunnel(
    listener: TcpListener,
    peer: IpAddr,
    key: [u8; 32],
    credentials: Arc<Credentials>,
    commands: mpsc::Sender<Control>,
    vehicle: Arc<crate::vehicle::Feed>,
) -> io::Result<()> {
    let mut socket = accept_peer(listener, peer).await?;
    let endpoint = credentials
        .endpoint
        .as_ref()
        .ok_or_else(|| err("wireless endpoint missing"))?;
    let start = Instant::now();
    let mut bootstrap = bounded(f50_core::bootstrap::Bootstrap::new(true, 0))?;
    let mut vehicle_updates=vehicle.subscribe();
    let initial=vehicle.snapshot();bounded(bootstrap.set_vehicle(initial.enabled,initial.values,0))?;
    let mut decoder = f50_core::tunnel::Tunnel::new(key);
    let mut buffer = [0; 16384];
    loop {
        let output = bootstrap.output();
        if !output.is_empty() {
            tokio::time::timeout(Duration::from_secs(3), commands.send(Control::Iap(output)))
                .await
                .map_err(err)?
                .map_err(err)?;
        }
        let now = start.elapsed().as_millis() as u64;
        let wait = bootstrap
            .deadline()
            .map(|d| Duration::from_millis(d.saturating_sub(now)))
            .unwrap_or(Duration::from_secs(30));
        tokio::select! {
         update=vehicle_updates.changed()=>{update.map_err(err)?;let snapshot=vehicle_updates.borrow_and_update().clone();bounded(bootstrap.set_vehicle(snapshot.enabled,snapshot.values,start.elapsed().as_millis() as u64))?;},
         n=socket.read(&mut buffer)=>{let n=n?;if n==0{return Err(err("iAP tunnel EOF"))}let now=start.elapsed().as_millis() as u64;bounded(decoder.feed(&buffer[..n],|bytes|{for part in bytes.chunks(65535){bootstrap.feed(part,endpoint,credentials.mfi.as_ref(),now).inspect_err(|e|crate::diag_echo!(Warn, "carplay", "tunnel_bootstrap={e:?} stage={}",bootstrap.stage()))?;}Ok(())}))?;},
         _=tokio::time::sleep(wait)=>{bounded(bootstrap.advance(start.elapsed().as_millis() as u64))?;}
        }
    }
}
async fn audio_stream(
    data: UdpSocket,
    control: UdpSocket,
    peer: IpAddr,
    key: [u8; 32],
    format: f50_core::audio::Format,
    kind: u8,
    hub: Arc<Hub>,
) -> io::Result<()> {
    let mut decoder = f50_core::audio::Decoder::new(key, format);
    let mut buffer = [0; 4097];
    let mut rtcp = [0; 4097];
    let mut failures = 0u8;
    loop {
        tokio::select! {
         packet=data.recv_from(&mut buffer)=>{let(n,source)=packet?;if source.ip()!=peer{continue}match decoder.decode(&buffer[..n],kind){Ok(Some(wire))=>{failures=0;hub.audio_received(kind,u32::from_be_bytes(buffer[4..8].try_into().unwrap()),u64::from_le_bytes(wire[2..10].try_into().unwrap()));hub.audio_packet(wire)},Ok(None)=>{},Err(_)=>{failures+=1;if failures>=8{return Err(err("audio authentication or framing failure limit"))}}}},
         packet=control.recv_from(&mut rtcp)=>{let(_,source)=packet?;if source.ip()!=peer{continue} // No RTP retransmission or RTCP clock steering implemented yet.
         }
        }
    }
}
async fn screen_stream(
    listener: TcpListener,
    peer: IpAddr,
    key: [u8; 32],
    hub: Arc<Hub>,
    width: u16,
    height: u16,
) -> io::Result<()> {
    let mut socket = accept_peer(listener, peer).await?;
    let mut screen = ScreenDecoder::new(key);
    let start = Instant::now();
    let mut configured = false;
    let mut body = Vec::new();
    loop {
        let mut header = [0; 128];
        tokio::time::timeout(Duration::from_secs(15), socket.read_exact(&mut header))
            .await
            .map_err(err)??;
        let n = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        if n > f50_core::media::MAX_MEDIA || (header[4] == 1 && n > 65536) {
            return Err(err("screen payload limit"));
        }
        body.resize(n, 0);
        tokio::time::timeout(Duration::from_secs(15), socket.read_exact(&mut body))
            .await
            .map_err(err)??;
        match bounded(screen.decode_in_place(&header, &mut body).inspect_err(|e| {
            crate::diag_echo!(Warn, "carplay",
                "screen_decode={e:?} opcode={} bytes={n}",
                header[4]
            )
        }))? {
            ScreenPacketKind::Control => continue,
            ScreenPacketKind::Config => {
                if body.len() > 65536 {
                    return Err(err("codec configuration limit"));
                }
                // H.264 avcC or, when HEVC was offered, an hvcC record (PlayPort passthrough).
                let (codec, config) = if let Some(at) = body.windows(4).position(|w| w == b"hvcC") {
                    (2u8, body[at + 4..].to_vec())
                } else if let Some(at) = body.windows(4).position(|w| w == b"avcC") {
                    (1u8, body[at + 4..].to_vec())
                } else {
                    (1u8, body.clone())
                };
                if config.len() < 7 || config[0] != 1 {
                    return Err(err("only H264 avcC or HEVC hvcC is supported"));
                }
                if codec == 2 {
                    crate::diag::diag().event("ok", "video", "iPhone 选择了 H.265/HEVC");
                }
                let mut wire = vec![1, 110, codec];
                wire.extend((width as u32).to_le_bytes());
                wire.extend((height as u32).to_le_bytes());
                wire.extend(config);
                hub.set_config(wire).map_err(err)?;
                configured = true;
            }
            ScreenPacketKind::Frame => {
                if !configured {
                    return Err(err("video frame before codec configuration"));
                }
                hub.publish_reusable(&mut body, start.elapsed().as_micros() as u64);
                hub.set_status("AUTHENTICATED_VIDEO_RECEIVED; BROWSER_DECODE_NOT_CONFIRMED");
            }
        }
    }
}
/// `setNightMode` command (PlayPort/DiPlay AirPlaySession.setNightMode).
fn night_command(night: bool) -> Value {
    dict([
        ("type", Value::String("setNightMode".into())),
        ("params", dict([("nightMode", Value::Boolean(night))])),
    ])
}
/// Event-channel commands for one queued control. Momentary keys always include release.
fn event_commands(control: Control) -> Vec<Value> {
    let hid = |uuid: &str, report: Vec<u8>| {
        dict([
            ("type", Value::String("hidSendReport".into())),
            ("uuid", Value::String(uuid.into())),
            ("hidReport", Value::Data(report)),
        ])
    };
    vec![match control {
        Control::Home => return vec![
            hid(f50_core::info::NAV_HID_UUID, vec![2, 0, 0, 0]),
            hid(f50_core::info::NAV_HID_UUID, vec![0; 4]),
        ],
        Control::Knob(report) => return vec![
            hid(f50_core::info::NAV_HID_UUID, report.to_vec()),
            hid(f50_core::info::NAV_HID_UUID, vec![0; 4]),
        ],
        Control::Media(index) => return vec![
            hid(f50_core::info::MEDIA_HID_UUID, vec![index]),
            hid(f50_core::info::MEDIA_HID_UUID, vec![0]),
        ],
        Control::Siri => return [2, 3].map(|action| dict([
            ("type", Value::String("requestSiri".into())),
            ("params", dict([("siriAction", number(action))])),
        ])).to_vec(),
        Control::Touch(report) => hid("2a2a2a2a", report.to_vec()),
        Control::ViewerDisconnected => hid("2a2a2a2a", vec![0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0]),
        Control::Iap(data) => dict([
            ("type", Value::String("iAPSendMessage".into())),
            ("params", dict([("data", Value::Data(data))])),
        ]),
        Control::Keyframe => dict([("type", Value::String("forceKeyFrame".into()))]),
        Control::NightMode(on) => night_command(on),
        Control::EndSession => return vec![],
    }]
}
/// RTSP `POST /command` carrying `command` as a binary plist, numbered with the next CSeq.
fn command_request(seq: &mut u32, command: &Value) -> io::Result<Vec<u8>> {
    let body = bounded(bplist::encode(command))?;
    *seq = seq.checked_add(1).ok_or_else(|| err("event sequence exhausted"))?;
    let mut wire = format!(
        "POST /command RTSP/1.0\r\nContent-Type: application/x-apple-binary-plist\r\nContent-Length: {}\r\nCSeq: {seq}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    wire.extend(body);
    Ok(wire)
}
async fn event_stream(
    listener: TcpListener,
    peer: IpAddr,
    mut cipher: ControlCipher,
    mut commands: mpsc::Receiver<Control>,
    night: bool,
) -> io::Result<()> {
    let mut socket = accept_peer(listener, peer).await?;
    let mut seq = 0u32;
    // Same order as PlayPort: the head unit's appearance is announced as soon as events exist.
    let wire = command_request(&mut seq, &night_command(night))?;
    write(&mut socket, &bounded(cipher.encrypt(&wire))?).await?;
    let mut buffer = [0; 4096];
    let mut encrypted = Vec::new();
    let mut decoder = Decoder::default();
    let mut request_categories = std::collections::BTreeSet::new();
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(control) = command else { return Ok(()) };
                for command in event_commands(control) {
                    let wire = command_request(&mut seq, &command)?;
                    write(&mut socket, &bounded(cipher.encrypt(&wire))?).await?;
                }
            }
            n = socket.read(&mut buffer) => {
                let n = n?;
                if n == 0 {
                    return Ok(());
                }
                if encrypted.len() + n > 1048576 {
                    return Err(err("event input limit"));
                }
                encrypted.extend(&buffer[..n]);
                let (plain, used) = bounded(cipher.decrypt(&encrypted))?;
                encrypted.drain(..used);
                bounded(decoder.feed(&plain))?;
                while let Some(request) = bounded(decoder.next_message())? {
                    if request.method.starts_with("RTSP/") || request.method.starts_with("HTTP/") {
                        continue;
                    }
                    let category = match (request.method.as_str(), request.path.as_str()) {
                        ("SETUP", _) => "SETUP",
                        ("TEARDOWN", _) => "TEARDOWN",
                        ("POST", "/command") => "COMMAND",
                        _ => "OTHER",
                    };
                    let plist = bplist::decode(&request.body).ok();
                    let parse = if request.body.is_empty() { "EMPTY" } else if plist.is_some() { "VALID" } else { "INVALID" };
                    let command = if category == "COMMAND" { plist.as_ref().map(command_category).unwrap_or("INVALID_PLIST") } else { "NONE" };
                    let audio_streams = plist.as_ref().is_some_and(audio_streams_present);
                    if request_categories.insert((category, command, parse, audio_streams)) {
                        crate::diag_echo!(Debug, "audio", "channel=EVENT request={category} command={command} plist={parse} audio_streams={audio_streams}");
                    }
                    let wire = bounded(response(&request, 200, "application/octet-stream", &[]))?;
                    write(&mut socket, &bounded(cipher.encrypt(&wire))?).await?;
                }
            }
        }
    }
}
async fn timing_loop(socket: UdpSocket, peer: SocketAddr, hub: Arc<Hub>) -> io::Result<()> {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut buffer = [0; 2048];
    loop {
        tokio::select! {
         _=tick.tick(),if peer.port()!=0=>{let mut request=[0;32];request[..4].copy_from_slice(&[0x80,210,0,7]);request[24..].copy_from_slice(&hub.clock.lock().unwrap().request().to_be_bytes());socket.send_to(&request,peer).await?;},
         packet=socket.recv_from(&mut buffer)=>{let(n,source)=packet?;if source!=peer||n!=32||buffer[0]!=0x80||buffer[2..4]!=[0,7]{continue}
          if buffer[1]==210{let mut reply=[0;32];reply[..4].copy_from_slice(&[0x80,211,0,7]);reply[8..16].copy_from_slice(&buffer[24..32]);let t=hub.clock.lock().unwrap().now().to_be_bytes();reply[16..24].copy_from_slice(&t);reply[24..32].copy_from_slice(&t);socket.send_to(&reply,source).await?;}
          else if buffer[1]==211{let mut clock=hub.clock.lock().unwrap();let t4=clock.local();let t=|i|u64::from_be_bytes(buffer[i..i+8].try_into().unwrap());clock.response(t(8),t(16),t(24),t4);}
         }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn setup_codec_agrees_with_info_and_preserves_negotiated_channels() {
        let phone = dict([("features", Value::Array(vec![
            Value::String(crate::ultra::FEATURE.into()), Value::String("unknownFeature".into()),
        ]))]);
        for hevc in [false, true] {
            let display = f50_core::info::DisplayInfo {
                name: "synthetic", model: "F50", manufacturer: "test",
                device_id: "02:00:00:00:00:01", bluetooth_mac: "02:00:00:00:00:01",
                width: 1280, height: 720, fps: 30, width_mm: 180, hevc, microphone: false, right_hand_drive: false,
            };
            let info = bplist::decode(&f50_core::info::build(&display).unwrap()).unwrap();
            let features = control_features(hevc, true, true, phone.as_dictionary().unwrap());
            let has = |name| features.iter().any(|v| v.as_string() == Some(name));
            assert_eq!(has("hevc"), info.as_dictionary().unwrap().contains_key("hevcInfo"),
                "SETUP must confirm the codec declared in /info");
            assert!(has("viewAreas") && has("iAPChannel") && has(crate::ultra::FEATURE));
            assert!(!has("unknownFeature"));
        }
        let features = control_features(false, false, false, phone.as_dictionary().unwrap());
        assert_eq!(features, vec![Value::String("viewAreas".into())]);
        let features = control_features(false, true, true, &Default::default());
        assert!(!features.iter().any(|v| v.as_string() == Some(crate::ultra::FEATURE)),
            "Ultra cannot be enabled without the phone offer");
    }
    #[test]
    fn ultra_setup_accepts_two_distinct_type_130_channels_and_rejects_duplicates() {
        use super::*;
        let channel = |uuid: &str| dict([
            ("type", number(130)), ("clientTypeUUID", Value::String(uuid.into())), ("seed", number(1)),
        ]);
        let high = channel("FF4A6720-F2BE-4F56-A3E1-DB3B4E37D634");
        let normal = channel("3E2F3C61-AAD0-42CB-A8AA-BF22186DA62E");
        let iap = channel("E9459FD0-BCAD-4C45-820F-1E72447EF2F2");
        assert!(validate_streams(&[high.clone(), normal.clone(), iap.clone()], true).is_ok());
        let duplicate = channel("ff4a6720-f2be-4f56-a3e1-db3b4e37d634");
        assert!(validate_streams(&[high, duplicate], true).is_err());
        assert!(validate_streams(&[dict([("type", number(110))]), dict([("type", number(110))])], true).is_err());
    }
    use super::*;
    use f50_core::crypto::{nonce, seal};
    struct NoIdentity;
    impl Identity for NoIdentity {
        fn certificate(&self)->&[u8] {&[]}
        fn sign_digest(&self,_:&[u8;32])->f50_core::Result<[u8;64]> {Err(f50_core::Error::Authentication)}
    }
    #[tokio::test]
    async fn phone_switch_closes_old_tcp_and_rejects_reentry_even_with_full_control_queue() {
        let a="/org/bluez/hci0/dev_00_11_22_33_44_55";let b="/org/bluez/hci0/dev_00_11_22_33_44_66";
        let phones=crate::phones::Phones::new(None,None);phones.initialize(Some(a)).unwrap();assert!(phones.handoff(0,a));
        let (hub,commands)=Hub::with_phones("synthetic".into(),"https://test.invalid".into(),phones);
        let credentials=Arc::new(Credentials{pairing_store:None,endpoint:None,pairing_id:b"synthetic-accessory".to_vec(),signing_seed:Zeroizing::new([0;32]),mfi:Arc::new(NoIdentity),info:Vec::new(),info_base:InfoBase{name:"synthetic".into(),model:"synthetic".into(),manufacturer:"synthetic".into(),device_id:"00:11:22:33:44:55".into(),bluetooth_mac:"00:11:22:33:44:55".into()},width:800,height:480});
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
        let (stop,stopped)=watch::channel(false);
        let running=tokio::spawn(Receiver{credentials,hub:hub.clone(),pairing_window:Duration::from_secs(120)}.run(listener,commands,stopped));
        let mut old=TcpStream::connect(address).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1),async{while !hub.has_phone_session(){tokio::task::yield_now().await;}}).await.unwrap();
        for _ in 0..32 {hub.control.try_send(Control::Keyframe).unwrap();}
        assert!(hub.control.try_send(Control::Keyframe).is_err(),"regression must exercise a saturated control queue");
        let epoch=hub.pause_phone().unwrap();let mut byte=[0];
        assert!(matches!(tokio::time::timeout(Duration::from_secs(1),old.read(&mut byte)).await.unwrap(),Ok(0)|Err(_)));
        tokio::time::timeout(Duration::from_secs(1),async{while hub.has_phone_session(){tokio::task::yield_now().await;}}).await.unwrap();
        let mut refused=TcpStream::connect(address).await.unwrap();assert_eq!(tokio::time::timeout(Duration::from_secs(1),refused.read(&mut byte)).await.unwrap().unwrap(),0);
        hub.phones.select(hub.phones.prepare(b).unwrap(),epoch,||Ok(())).unwrap();assert!(hub.phones.handoff(epoch,b));
        let _new=TcpStream::connect(address).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1),async{while !hub.has_phone_session(){tokio::task::yield_now().await;}}).await.unwrap();
        stop.send(true).unwrap();running.await.unwrap().unwrap();assert!(!hub.has_phone_session());
    }
    #[tokio::test(start_paused = true)]
    async fn ready_keepalive_at_control_deadline_does_not_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _phone = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut control, _) = listener.accept().await.unwrap();
        let (activity, mut receiver) = watch::channel(tokio::time::Instant::now());
        let mut buffer = [0; 1];
        let mut reading = Box::pin(read_control(&mut control, &mut buffer, &mut receiver));
        assert!(futures_util::poll!(reading.as_mut()).is_pending());
        tokio::time::advance(Duration::from_secs(60)).await;
        activity.send(tokio::time::Instant::now()).unwrap();
        assert!(
            futures_util::poll!(reading.as_mut()).is_pending(),
            "a valid keepalive already waiting at the timeout boundary must renew the session"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn keepalive_renews_silent_control_but_total_silence_still_expires() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _phone = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut control, _) = listener.accept().await.unwrap();
        let (activity, mut receiver) = watch::channel(tokio::time::Instant::now());
        let reading =
            tokio::spawn(
                async move { read_control(&mut control, &mut [0; 1], &mut receiver).await },
            );
        tokio::task::yield_now().await;
        for _ in 0..3 {
            tokio::time::advance(Duration::from_secs(40)).await;
            activity.send(tokio::time::Instant::now()).unwrap();
            tokio::task::yield_now().await;
            assert!(!reading.is_finished(), "valid keepalive must preserve a TCP-silent session across the original 60-second deadline");
        }
        tokio::time::advance(Duration::from_secs(61)).await;
        assert!(
            reading.await.unwrap().is_err(),
            "silence on TCP and keepalive must still expire the session"
        );
    }
    #[tokio::test]
    async fn requested_keepalive_port_is_live_and_owned_by_the_control_session() {
        let local: IpAddr = "127.0.0.1".parse().unwrap();
        let phone = UdpSocket::bind(SocketAddr::new(local, 0)).await.unwrap();
        let base = dict([("eventPort", number(12345)), ("timingPort", number(12346))]);
        for requested in [false, true] {
            let mut tasks = JoinSet::new();
            let (activity, mut activity_rx) = watch::channel(tokio::time::Instant::now());
            let bytes =
                control_setup_response(base.clone(), requested, local, local, &mut tasks, activity)
                    .await
                    .unwrap();
            let reply = bplist::decode(&bytes).unwrap();
            let fields = reply.as_dictionary().unwrap();
            if !requested {
                assert!(!fields.contains_key("keepAlivePort"));
                assert!(tasks.is_empty());
                assert_eq!(reply, base);
                continue;
            }
            let port = fields
                .get("keepAlivePort")
                .and_then(Value::as_unsigned_integer)
                .expect("phone requested a keepalive transport but SETUP omitted its port");
            assert!((1..=65535).contains(&port));
            assert_eq!(fields["eventPort"], number(12345));
            assert_eq!(fields["timingPort"], number(12346));
            let address = SocketAddr::new(local, port as u16);
            assert!(
                UdpSocket::bind(address).await.is_err(),
                "advertised port must remain bound"
            );
            phone.send_to(&[0x20], address).await.unwrap();
            tokio::time::timeout(Duration::from_secs(1), activity_rx.changed())
                .await
                .unwrap()
                .unwrap();
            let mut packet = [0; 32];
            tokio::select! {
                packet = phone.recv_from(&mut packet) => panic!("beacons must not receive a reply: {packet:?}"),
                finished = tasks.join_next() => panic!("receiving a beacon ended the session task: {finished:?}"),
                _ = tokio::time::sleep(Duration::from_millis(40)) => {}
            }
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
            assert!(
                UdpSocket::bind(address).await.is_ok(),
                "session cancellation must release its UDP port"
            );
        }
    }
    #[tokio::test]
    async fn udp_audio_decrypts_into_browser_queue() {
        let data = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = data.local_addr().unwrap();
        let control = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let phone = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (h, _) = Hub::new(
            "synthetic-token-for-tests-only".into(),
            "https://test.invalid".into(),
        );
        h.viewer.store(true, std::sync::atomic::Ordering::Relaxed);
        let task = tokio::spawn(audio_stream(
            data,
            control,
            addr.ip(),
            [7; 32],
            f50_core::audio::Format::from_bits(0x4000).unwrap(),
            101,
            h.clone(),
        ));
        let mut wire = vec![0x80, 96, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1];
        let payload = seal(&[7; 32], &nonce(0), &[0x12, 0x34], &wire[4..12]).unwrap();
        wire.extend(payload);
        wire.extend(0u64.to_le_bytes());
        phone.send_to(&wire, addr).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let packet = h.audio.lock().unwrap().packets.pop_front();
                if let Some(p) = packet {
                    assert_eq!(&p[10..], &[0x34, 0x12]);
                    break;
                }
                h.notify.notified().await;
            }
        })
        .await
        .unwrap();
        task.abort();
    }
    #[tokio::test]
    async fn encrypted_tcp_screen_to_bounded_hub() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (h, _) = Hub::new(
            "synthetic-token-for-tests-only".into(),
            "https://test.invalid".into(),
        );
        h.viewer.store(true, std::sync::atomic::Ordering::Relaxed);
        let hh = h.clone();
        let task = tokio::spawn(screen_stream(listener, addr.ip(), [9; 32], hh, 800, 480));
        let mut client = TcpStream::connect(addr).await.unwrap();
        let avcc = [1, 66, 0, 31, 255, 225, 0, 1, 103];
        let mut header = [0; 128];
        header[..4].copy_from_slice(&(avcc.len() as u32).to_le_bytes());
        header[4] = 1;
        client.write_all(&header).await.unwrap();
        client.write_all(&avcc).await.unwrap();
        header = [0; 128];
        header[..4].copy_from_slice(&22u32.to_le_bytes());
        let frame = seal(&[9; 32], &nonce(0), &[0, 0, 0, 2, 0x65, 1], &header).unwrap();
        client.write_all(&header).await.unwrap();
        client.write_all(&frame).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let frame = h.queue.lock().unwrap().pop();
                if let Some(f) = frame {
                    assert_eq!(f.data, vec![0, 0, 0, 1, 0x65, 1]);
                    assert!(f.key);
                    break;
                }
                h.notify.notified().await;
            }
        })
        .await
        .unwrap();
        let mut control_header = [0; 128];
        control_header[4] = 2;
        client.write_all(&control_header).await.unwrap();
        let second = seal(&[9; 32], &nonce(1), &[0, 0, 0, 2, 0x65, 2], &header).unwrap();
        client.write_all(&header).await.unwrap();
        client.write_all(&second).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(frame) = h.queue.lock().unwrap().pop() {
                    assert_eq!(frame.data, vec![0, 0, 0, 1, 0x65, 2]);
                    break;
                }
                h.notify.notified().await;
            }
        })
        .await
        .unwrap();
        drop(client);
        assert!(task.await.unwrap().is_err());
    }
    #[tokio::test]
    async fn actual_encrypted_hid_event_wire() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel(32);
        let task = tokio::spawn(event_stream(
            listener,
            addr.ip(),
            ControlCipher::new([1; 32], [2; 32]),
            rx,
            false,
        ));
        let mut phone = TcpStream::connect(addr).await.unwrap();
        tx.send(Control::Touch([3; 12])).await.unwrap();
        tx.send(Control::Home).await.unwrap();
        tx.send(Control::NightMode(true)).await.unwrap();
        tx.send(Control::Knob([4, 129, 127, 0])).await.unwrap();
        tx.send(Control::Media(3)).await.unwrap();
        tx.send(Control::Siri).await.unwrap();
        let mut cipher = ControlCipher::new([2; 32], [1; 32]);
        let mut parser = Decoder::default();
        let mut encrypted = Vec::new();
        let mut commands = Vec::new();
        let mut buf = [0; 4096];
        while commands.len() < 11 {
            let n = tokio::time::timeout(Duration::from_secs(2), phone.read(&mut buf))
                .await
                .unwrap()
                .unwrap();
            encrypted.extend_from_slice(&buf[..n]);
            let (plain, used) = cipher.decrypt(&encrypted).unwrap();
            encrypted.drain(..used);
            parser.feed(&plain).unwrap();
            while let Some(request) = parser.next_message().unwrap() {
                assert_eq!(request.path, "/command");
                commands.push(bplist::decode(&request.body).unwrap());
            }
        }
        // Appearance is announced first, then queued commands keep their order.
        let night = commands[0].as_dictionary().unwrap();
        assert_eq!(night["type"].as_string(), Some("setNightMode"));
        assert_eq!(night["params"].as_dictionary().unwrap()["nightMode"].as_boolean(), Some(false));
        let d = commands[1].as_dictionary().unwrap();
        assert_eq!(d["type"].as_string(), Some("hidSendReport"));
        assert_eq!(d["hidReport"].as_data(), Some([3u8; 12].as_slice()));
        for (index, report) in [(2, [2,0,0,0]), (3, [0,0,0,0])] {
            let d = commands[index].as_dictionary().unwrap();
            assert_eq!(d["type"].as_string(), Some("hidSendReport"));
            assert_eq!(d["uuid"].as_string(), Some(f50_core::info::NAV_HID_UUID));
            assert_eq!(d["hidReport"].as_data(), Some(report.as_slice()), "Home press and release stay together before the following control");
        }
        let n = commands[4].as_dictionary().unwrap();
        assert_eq!(n["params"].as_dictionary().unwrap()["nightMode"].as_boolean(), Some(true));
        for (index,uuid,report) in [(5,f50_core::info::NAV_HID_UUID,vec![4,129,127,0]),
            (6,f50_core::info::NAV_HID_UUID,vec![0;4]),(7,f50_core::info::MEDIA_HID_UUID,vec![3]),
            (8,f50_core::info::MEDIA_HID_UUID,vec![0])] {
            let d=commands[index].as_dictionary().unwrap();
            assert_eq!(d["type"].as_string(),Some("hidSendReport"));
            assert_eq!(d["uuid"].as_string(),Some(uuid));
            assert_eq!(d["hidReport"].as_data(),Some(report.as_slice()));
        }
        for (index,action) in [(9,2),(10,3)] {
            let d=commands[index].as_dictionary().unwrap();
            assert_eq!(d["type"].as_string(),Some("requestSiri"));
            assert_eq!(d["params"].as_dictionary().unwrap()["siriAction"].as_unsigned_integer(),Some(action));
        }
        drop(phone);
        assert!(task.await.unwrap().is_ok());
    }
}
