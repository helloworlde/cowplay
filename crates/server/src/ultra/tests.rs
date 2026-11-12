use super::*;
use livi::opack::{self, dict, Value};

fn request(command: &str, values: Value) -> Vec<u8> {
    opack::encode(&dict([
        ("pluginID", Value::Int(0)),
        ("pluginData", Value::Data(opack::encode(&dict([
            ("command", Value::str(command)),
            ("transactionID", Value::Int(42)),
            ("values", values),
        ])))),
    ]))
}

fn opened(bytes: &[u8]) -> Value {
    let outer = opack::decode(bytes).unwrap();
    opack::decode(outer.get("pluginData").and_then(Value::as_data).unwrap()).unwrap()
}
fn iid(values: &Value, id: i64) -> Option<&Value> { values.as_dict()?.iter().find(|(key,_)| key.as_int()==Some(id)).map(|(_,v)|v) }

fn climate_reading(feed: &feed::Feed) {
    let now = feed::now_ms();
    feed.publish_climate(true, Some(f50_core::vehicle::ClimateState {
        hvac_on: Some(false), driver_temp_celsius: Some(21.0), passenger_temp_celsius: Some(22.0),
        min_temp_celsius: Some(15.0), max_temp_celsius: Some(28.0), sampled_ms: Some(now), ..Default::default()
    }), Some(now));
}

fn climate_description(probe: &Probe) -> Value {
    let answers = probe.state.lock().unwrap().handle(
        Lane::High, &request("configRequest", Value::Null),
    );
    let response = opened(&answers[0].1);
    response.get("values").unwrap().get("accessories").unwrap().as_array().unwrap()
        .iter().find(|a| a.get("iid").and_then(Value::as_int) == Some(512)).unwrap().clone()
}

fn climate_characteristic(description: &Value, id: i64) -> &Value {
    description.get("services").unwrap().as_array().unwrap().iter()
        .flat_map(|s| s.get("characteristics").unwrap().as_array().unwrap())
        .find(|c| c.get("iid").and_then(Value::as_int) == Some(id))
        .unwrap_or_else(|| panic!("missing climate characteristic {id}"))
}

#[test]
fn offline_climate_panel_has_operable_control_values_without_vehicle_commands() {
    let probe = Probe::new();
    let description = climate_description(&probe);
    for id in [514, 519, 520, 533, 534] {
        assert!(climate_characteristic(&description, id).get("initialValue").is_some(),
            "the native control {id} must remain constructible offline");
    }
    let reply = opened(&probe.state.lock().unwrap().handle(Lane::High,
        &request("readRequest", Value::Array([514,519,520,533,534].map(Value::Int).to_vec())))[0].1);
    for id in [519,533] { assert_eq!(iid(reply.get("values").unwrap(),id),Some(&Value::Bool(true))); }
    assert!(probe.feed.climate().current().is_none(), "UI selections are never telemetry");
}

#[tokio::test]
async fn climate_feedback_follows_attempts_and_driver_readiness_not_monitor_connection() {
    let feed=feed::Feed::new();
    let mut commands=feed.take_climate_requests().unwrap();
    let probe=Probe::with_feed(feed.clone());
    let (sender,mut replies)=mpsc::channel(16);
    probe.outgoing.lock().unwrap().insert(0,sender);
    assert_eq!(probe.climate_control()["feedback"]["sequence"],0);
    let body=request("writeRequest",Value::Dict(vec![(Value::Int(514),Value::Bool(true))]));
    assert!(probe.native_write(0,&body).unwrap().unwrap().is_none());
    let response=opened(&replies.recv().await.unwrap());
    assert_ne!(iid(response.get("errors").unwrap(),514),Some(&Value::Int(0)));
    assert!(probe.climate_control()["feedback"]["message"].as_str().unwrap().contains("车辆未连接"));
    assert!(commands.try_recv().is_err());
    climate_reading(&feed); // Driver is ready while the read-only Monitor is paused.
    assert!(!feed.snapshot().connected);
    assert!(probe.native_write(0,&body).unwrap().unwrap().is_none());
    replies.recv().await.unwrap();
    assert!(probe.climate_control()["feedback"]["message"].as_str().unwrap().contains("温控未授权"));
    assert!(commands.try_recv().is_err());
    assert_eq!(probe.climate_control()["feedback"]["sequence"],2);
}

#[test]
fn live_climate_disconnected_temperature_controls_keep_valid_ranges() {
    let feed = feed::Feed::new();
    let probe = Probe::with_feed(feed.clone());
    for stage in 0..3 {
        if stage == 1 { climate_reading(&feed); }
        if stage == 2 { feed.reset_climate(); }
        probe.refresh().unwrap();
        let description = climate_description(&probe);
        for id in [520, 534] {
            let c = climate_characteristic(&description, id);
            let real = |name| match c.get(name) {
                Some(Value::Real(v)) if v.is_finite() => *v,
                other => panic!("stage {stage}: temperature {id} needs {name}, got {other:?}"),
            };
            assert!(real("minimumValue") < real("maximumValue"));
            assert!(real("stepValue") > 0.0);
            if stage != 1 {
                assert!(matches!(c.get("initialValue"), Some(Value::Real(v)) if v.is_finite()), "offline target is an editable preset");
                assert!(probe.feed.climate().current().is_none(), "preset is not a vehicle reading");
            }
        }
    }
}

#[test]
fn live_climate_temperature_zones_keep_read_only_on_state() {
    let feed = feed::Feed::new();
    let probe = Probe::with_feed(feed.clone());
    for stage in 0..3 {
        if stage == 1 { climate_reading(&feed); }
        if stage == 2 { feed.reset_climate(); }
        probe.refresh().unwrap();
        let description = climate_description(&probe);
        for id in [519, 533] {
            let c = climate_characteristic(&description, id);
            assert_eq!(c.get("writable"), Some(&Value::Bool(false)), "zone state is not a new control");
            assert_eq!(c.get("type"), Some(&Value::Int(0x3000_0002)));
            let reply = opened(&probe.state.lock().unwrap().handle(Lane::High,
                &request("readRequest", Value::Array(vec![Value::Int(id)])))[0].1);
            assert_eq!(iid(reply.get("values").unwrap(), id), Some(&Value::Bool(true)),
                "temperature selection stays enabled even with HVAC off or vehicle offline");
            let reply = opened(&probe.state.lock().unwrap().handle(Lane::High,
                &request("writeRequest", Value::Dict(vec![(Value::Int(id), Value::Bool(true))])))[0].1);
            assert_eq!(iid(reply.get("errors").unwrap(), id), Some(&Value::Int(-6727)));
        }
    }
}

#[test]
fn native_climate_rejects_unknown_stale_missing_and_mixed_controls() {
    let feed=feed::Feed::new(); climate_reading(&feed);
    let snapshot=feed.climate(); let current=snapshot.current().unwrap();
    let write=|values| request("writeRequest",Value::Dict(values));
    let one=write(vec![(Value::Int(520),Value::Real(23.5))]);
    assert_eq!(VehicleState::live_climate_write(&one,Some(current)).unwrap().1,Ok(ClimateCommand::SetTemperature{driver_celsius:23.5,passenger_celsius:22.0}));
    assert!(VehicleState::live_climate_write(&one,None).unwrap().1.is_err());
    let missing=f50_core::vehicle::ClimateState{passenger_temp_celsius:None,..current.clone()};
    assert!(VehicleState::live_climate_write(&one,Some(&missing)).unwrap().1.is_err());
    for values in [
        vec![(Value::Int(520),Value::Real(100.0))],
        vec![(Value::Int(520),Value::str("23"))],
        vec![(Value::Int(520),Value::Real(23.0)),(Value::Int(520),Value::Real(24.0))],
        vec![(Value::Int(514),Value::Bool(true)),(Value::Int(520),Value::Real(23.0))],
        vec![(Value::Int(520),Value::Real(23.0)),(Value::Int(525),Value::Int(5))],
    ] { assert!(VehicleState::live_climate_write(&write(values),Some(current)).unwrap().1.is_err()); }
    let probe=Probe::with_feed(feed); let mut state=probe.state.lock().unwrap();
    let response=opened(&state.handle(Lane::High,&request("readRequest",Value::Array(vec![Value::Int(514),Value::Int(520),Value::Int(525)])))[0].1);
    assert_eq!(iid(response.get("values").unwrap(),520),Some(&Value::Real(21.0)));
    assert!(response.get("errors").is_some(),"unsupported fan has no fabricated value");
}

#[tokio::test]
async fn native_grants_bind_verified_session_and_vehicle_and_cancelled_transactions_do_not_repeat() {
    let feed=feed::Feed::new(); climate_reading(&feed);
    let mut requests=feed.take_climate_requests().unwrap();
    let probe=Probe::with_feed(feed.clone()); let (tx,mut replies)=mpsc::channel(16); probe.outgoing.lock().unwrap().insert(0,tx);
    let (hub,_)=crate::Hub::new("synthetic-token".into(),"https://car.example.test".into()); hub.set_ultra_probe(Some(&probe));
    let session=hub.media_generation(); let context=feed.climate().context;
    assert!(hub.set_ultra_climate_control(true,session,context).is_err());
    hub.phone_active.store(true,Ordering::Release);hub.phone_verified.store(true,Ordering::Release);
    let body=request("writeRequest",Value::Dict(vec![(Value::Int(514),Value::Bool(true))]));
    assert!(probe.native_write(0,&body).unwrap().unwrap().is_none());replies.recv().await.unwrap();assert!(requests.try_recv().is_err());
    hub.set_ultra_climate_control(true,session,context).unwrap();
    let pending=probe.native_write(0,&body).unwrap().unwrap().unwrap();let sent=requests.recv().await.unwrap();assert!(sent.valid(context));
    drop(pending);assert!(sent.reply.is_closed());drop(sent);
    probe.revoke_climate_control();hub.set_ultra_climate_control(true,session,context).unwrap();
    assert!(probe.native_write(0,&body).unwrap().unwrap().is_none());let replay=opened(&replies.recv().await.unwrap());assert_ne!(iid(replay.get("errors").unwrap(),514),Some(&Value::Int(0)));assert!(requests.try_recv().is_err(),"an uncertain transaction cannot execute twice");
    let mut fresh_write=opened(&body);let Value::Dict(fields)=&mut fresh_write else { unreachable!() };fields.iter_mut().find(|(k,_)| k.as_str()==Some("transactionID")).unwrap().1=Value::Int(43);
    let fresh_body=opack::encode(&dict([("pluginID",Value::Int(0)),("pluginData",Value::Data(opack::encode(&fresh_write)))]));
    let pending=probe.native_write(0,&fresh_body).unwrap().unwrap().unwrap();requests.recv().await.unwrap().reply.send(true).unwrap();
    probe.native_finished(0,pending,true).unwrap();let confirmed=opened(&replies.recv().await.unwrap());assert_eq!(iid(confirmed.get("errors").unwrap(),514),Some(&Value::Int(0)));
    assert_eq!(feed.climate().current().unwrap().hvac_on,Some(false),"a command ACK never invents the measured HVAC state");
    feed.publish_climate(true,None,None);
    assert!(probe.native_write(0,&fresh_body).unwrap().unwrap().is_none());let replay=opened(&replies.recv().await.unwrap());assert_eq!(iid(replay.get("errors").unwrap(),514),Some(&Value::Int(0)));assert!(requests.try_recv().is_err(),"confirmed responses replay even while actual state awaits readback");
    feed.reset_climate();climate_reading(&feed);
    assert!(hub.set_ultra_climate_control(true,session,context).is_err(),"stale vehicle confirmation rejected");
    hub.set_ultra_climate_control(true,session,feed.climate().context).unwrap();
    hub.pause_phone().unwrap();assert!(probe.native.borrow().is_none(),"phone pause revokes before socket cleanup");
    assert!(hub.set_ultra_climate_control(true,session,feed.climate().context).is_err(),"a pausing phone cannot be reauthorized");
    hub.reset();assert!(probe.native.borrow().is_none(),"a retained old Probe has no grant");
    assert!(hub.set_ultra_climate_control(true,session,feed.climate().context).is_err());
}

#[tokio::test]
async fn native_pending_keeps_same_lane_readable_and_eof_closes_the_vehicle_request() {
    let feed=feed::Feed::new();climate_reading(&feed);let mut requests=feed.take_climate_requests().unwrap();
    let probe=Probe::with_feed(feed.clone());
    let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let shared=[23;32];let seed=9;let salt=format!("DataStream-Salt{seed}");
    let mut cipher=ControlCipher::new(derive(&shared,salt.as_bytes(),b"DataStream-Input-Encryption-Key").unwrap(),derive(&shared,salt.as_bytes(),b"DataStream-Output-Encryption-Key").unwrap());
    let task=tokio::spawn(stream(listener,address.ip(),shared,seed,probe.clone(),0));let mut socket=TcpStream::connect(address).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1),async{while !probe.connected_channels()[0]{tokio::task::yield_now().await;}}).await.unwrap();
    probe.set_climate_control(true,feed.climate().context).unwrap();
    let write=request("writeRequest",Value::Dict(vec![(Value::Int(514),Value::Bool(true))]));
    socket.write_all(&cipher.encrypt(&package(ASYNC,COMMAND,0,&write).unwrap()).unwrap()).await.unwrap();
    let mut pending=tokio::time::timeout(Duration::from_secs(1),requests.recv()).await.unwrap().unwrap();
    socket.write_all(&cipher.encrypt(&package(ASYNC,COMMAND,0,&request("readRequest",Value::Array(vec![Value::Int(514)]))).unwrap()).unwrap()).await.unwrap();
    let response=receive(&mut socket,&mut cipher).await;assert_eq!(opened(&response[32..]).get("command").and_then(Value::as_str),Some("readResponse"));
    drop(socket);tokio::time::timeout(Duration::from_secs(1),task).await.unwrap().unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(1),pending.reply.closed()).await.unwrap();assert!(pending.grant.borrow().is_none());
}

#[test]
fn native_grant_does_not_hold_ultra_lock_while_waiting_for_phone_selection() {
    let (hub,_commands)=crate::Hub::new("synthetic-token".into(),"https://car.example.test".into());
    let probe=Probe::new();hub.set_ultra_probe(Some(&probe));
    hub.phone_active.store(true,Ordering::Release);hub.phone_verified.store(true,Ordering::Release);
    let epoch=hub.phones.epoch();let session=hub.media_generation();
    let (worker,clear)=hub.phones.while_current(epoch,||{
        let (started,ready)=std::sync::mpsc::channel();let granting=hub.clone();
        let worker=std::thread::spawn(move||{started.send(()).unwrap();granting.set_ultra_climate_control(true,session,0)});
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        let mut clear=true;
        for _ in 0..10_000 {
            if hub.ultra_probe.try_lock().is_err() {clear=false;break;}
            std::thread::yield_now();
        }
        if clear {assert!(hub.end_session(),"display commit can revoke while holding phone selection");}
        (worker,clear)
    }).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert!(clear,"grant must acquire phone state before the Ultra mutex");
}

async fn receive(socket: &mut TcpStream, cipher: &mut ControlCipher) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut sealed = Vec::new();
        let mut complete = Vec::new();
        loop {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            sealed.push(byte[0]);
            let (plain, used) = cipher.decrypt(&sealed).unwrap();
            if used > 0 {
                sealed.drain(..used);
                complete.extend(plain);
                if complete.len() >= 4 {
                    let size = u32::from_be_bytes(complete[..4].try_into().unwrap()) as usize;
                    if complete.len() >= size { return complete; }
                }
            }
        }
    }).await.expect("encrypted Ultra response")
}

fn ordinary_info() -> Vec<u8> {
    f50_core::info::build(&f50_core::info::DisplayInfo {
        name: "cowplay-rasp", model: "F50", manufacturer: "CowPlay",
        device_id: "02:00:00:00:00:01", bluetooth_mac: "02:00:00:00:00:01",
        width: 1280, height: 720, fps: 30, width_mm: 180, hevc: false, microphone: false, right_hand_drive: false,
    }).unwrap()
}

#[test]
fn default_and_explicit_runtime_modes_control_info_declaration() {
    let Ok(mode) = std::env::var("COWPLAY_ULTRA_MODE_TEST") else {
        for (mode, value) in [("default", None), ("enabled", Some("1")), ("disabled", Some("0"))] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "ultra::tests::default_and_explicit_runtime_modes_control_info_declaration"])
                .env("COWPLAY_ULTRA_MODE_TEST", mode).env_remove("COWPLAY_ULTRA_PROBE");
            if let Some(value) = value { child.env("COWPLAY_ULTRA_PROBE", value); }
            let result = child.output().unwrap();
            assert!(result.status.success(), "{mode}: {}{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
        }
        return;
    };
    let expected = mode != "disabled";
    assert_eq!(enabled(), expected, "{mode} runtime mode");
    let ordinary = ordinary_info();
    let actual = augment_info(ordinary.clone());
    if expected {
        let info = bplist::decode(&actual).unwrap();
        let fields = info.as_dictionary().unwrap();
        assert_eq!(fields["name"].as_string(), Some("cowplay-rasp"), "Ultra must preserve the receiver name");
        assert!(fields.contains_key("vehicleStateProtocolInfo"));
        assert_eq!(fields["displayPanels"].as_array().unwrap().len(), 2);
    } else {
        assert_eq!(actual, ordinary, "explicit disable must preserve ordinary CarPlay info");
    }
}

#[test]
fn live_profile_separates_control_presets_from_vehicle_readings() {
    let bytes = ordinary_info();
    let changed = Probe::new().augment_info(&bytes).unwrap();
    let changed = bplist::decode(&changed).unwrap();
    let fields = changed.as_dictionary().unwrap();
    assert_eq!(fields["name"].as_string(), Some("cowplay-rasp"), "Ultra must preserve the receiver name");
    assert_eq!(fields["displayPanels"].as_array().unwrap().len(), 2);
    let protocol = fields["vehicleStateProtocolInfo"].as_dictionary().unwrap();
    assert_eq!(protocol["protocolVersion"].as_string(), Some("1.0"));
    let probe = Probe::new();
    let description = climate_description(&probe);
    for id in [514, 519, 520, 533, 534] {
        assert!(climate_characteristic(&description, id).get("initialValue").is_some(),
            "native control {id} needs a UI preset");
    }
    assert!(probe.feed.climate().current().is_none());
    let services = description.get("services").unwrap().as_array().unwrap();
    assert_eq!(services.len(), 3, "only HVAC power and the two temperature zones are advertised");
    assert!(bplist::decode(&bytes).unwrap().as_dictionary().unwrap().get("vehicleStateProtocolInfo").is_none());
}

#[tokio::test]
async fn encrypted_phone_reads_read_only_state_and_receives_live_cross_lane_updates() {
    let feed=feed::Feed::new();
    feed.status(true,true,"reading");
    let probe = Probe::with_feed(feed.clone());
    let (hub, _commands) = crate::Hub::new("synthetic-token".into(), "https://car.example.test".into());
    hub.set_ultra_probe(Some(&probe));
    let shared = [23u8; 32];
    let seed = 123456u64;
    let salt = format!("DataStream-Salt{seed}");
    let phone_read = derive(&shared, salt.as_bytes(), b"DataStream-Input-Encryption-Key").unwrap();
    let phone_write = derive(&shared, salt.as_bytes(), b"DataStream-Output-Encryption-Key").unwrap();
    let mut sockets = Vec::new();
    let mut tasks = tokio::task::JoinSet::new();
    for lane in 0..2 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tasks.spawn(stream(listener, address.ip(), shared, seed, probe.clone(), lane));
        sockets.push(TcpStream::connect(address).await.unwrap());
    }
    let mut high = sockets.remove(0);
    let mut normal = sockets.remove(0);
    let mut high_cipher = ControlCipher::new(phone_read, phone_write);
    let mut normal_cipher = ControlCipher::new(phone_read, phone_write);
    let config = package(SYNC, COMMAND, 7, &request("configRequest", Value::Null)).unwrap();
    let encrypted = high_cipher.encrypt(&config).unwrap();
    for part in encrypted.chunks(3) { high.write_all(part).await.unwrap(); }
    let ack = receive(&mut high, &mut high_cipher).await;
    assert_eq!(&ack[4..8], &REPLY.to_be_bytes());
    assert_eq!(&ack[20..28], &7u64.to_be_bytes());
    let response = receive(&mut high, &mut high_cipher).await;
    let config = opened(&response[32..]);
    assert_eq!(config.get("command").and_then(Value::as_str), Some("configResponse"));
    let description = config.get("values").unwrap().get("accessories").unwrap().as_array().unwrap();
    assert!(description.len() > 2, "the phone must receive the actual vehicle service graph");

    high.write_all(&high_cipher.encrypt(&package(ASYNC, COMMAND, 0, &request("registerRequest", Value::Array(vec![Value::Int(1538)]))).unwrap()).unwrap()).await.unwrap();
    let registered = receive(&mut high, &mut high_cipher).await;
    assert_eq!(opened(&registered[32..]).get("command").and_then(Value::as_str), Some("registerResponse"));
    normal.write_all(&normal_cipher.encrypt(&package(ASYNC, COMMAND, 0, &request("writeRequest", Value::Dict(vec![(Value::Int(1538), Value::Int(99))]))).unwrap()).unwrap()).await.unwrap();
    let written = receive(&mut normal, &mut normal_cipher).await;
    assert_eq!(opened(&written[32..]).get("command").and_then(Value::as_str), Some("writeResponse"));
    let written=opened(&written[32..]);
    assert!(written.get("errors").unwrap().as_dict().unwrap().contains(&(Value::Int(1538),Value::Int(-6727))),"phone cannot write a sensor reading");
    let now=feed::now_ms();
    feed.apply(f50_core::vehicle::Update::Charge{soc_pct:Some(f50_core::vehicle::Reading{value:68.0,received_ms:now,sample_received_ms:now,sampled_ms:Some(now),sampled_time_trusted:true}),range_km:None});
    let updated = receive(&mut high, &mut high_cipher).await;
    let updated = opened(&updated[32..]);
    assert_eq!(updated.get("command").and_then(Value::as_str), Some("updateNotify"));
    assert!(updated.get("values").unwrap().as_dict().unwrap().contains(&(Value::Int(1538), Value::Int(68))));
    feed.status(true,false,"disconnected");
    let invalidated=receive(&mut high,&mut high_cipher).await;
    assert!(opened(&invalidated[32..]).get("values").unwrap().as_dict().unwrap().contains(&(Value::Int(1538),Value::Null)),"loss of source invalidates the subscribed value");
    high.write_all(&high_cipher.encrypt(&package(ASYNC, COMMAND, 0, &request("readRequest",Value::Array(vec![Value::Int(1538)]))).unwrap()).unwrap()).await.unwrap();
    let missing=receive(&mut high,&mut high_cipher).await;
    assert!(opened(&missing[32..]).get("errors").unwrap().as_dict().unwrap().contains(&(Value::Int(1538),Value::Int(-6727))));
    // Opening and revisiting climate without a vehicle must preserve both encrypted
    // channels, reject writes, and recover from an unavailable -> fresh -> lost feed.
    let climate_ids = [514, 519, 520, 533, 534];
    for command in ["registerRequest", "readRequest"] {
        let body = request(command, Value::Array(climate_ids.map(Value::Int).to_vec()));
        high.write_all(&high_cipher.encrypt(&package(ASYNC, COMMAND, 0, &body).unwrap()).unwrap()).await.unwrap();
        let reply = opened(&receive(&mut high, &mut high_cipher).await[32..]);
        for id in climate_ids { assert!(iid(reply.get("values").unwrap(), id).is_some(), "control {id} remains usable offline"); }
    }
    normal.write_all(&normal_cipher.encrypt(&package(ASYNC, COMMAND, 0,
        &request("writeRequest", Value::Dict(vec![(Value::Int(514), Value::Bool(true))]))).unwrap()).unwrap()).await.unwrap();
    let refused = opened(&receive(&mut normal, &mut normal_cipher).await[32..]);
    assert_eq!(iid(refused.get("errors").unwrap(), 514), Some(&Value::Int(-6727)));
    for fresh in [true, false, true] {
        if fresh { climate_reading(&feed); } else { feed.reset_climate(); }
        probe.refresh().unwrap();
        let body = request("readRequest",Value::Array(climate_ids.map(Value::Int).to_vec()));
        high.write_all(&high_cipher.encrypt(&package(ASYNC,COMMAND,0,&body).unwrap()).unwrap()).await.unwrap();
        let update = loop {
            let reply=opened(&receive(&mut high,&mut high_cipher).await[32..]);
            if reply.get("command").and_then(Value::as_str)==Some("readResponse") {break reply;}
        };
        assert_eq!(iid(update.get("values").unwrap(), 520), Some(&Value::Real(21.0)));
        for id in [519, 533] {
            assert_eq!(iid(update.get("values").unwrap(), id), Some(&Value::Bool(true)));
        }
        assert_eq!(feed.climate().current().is_some(),fresh);
    }
    assert_eq!(hub.ultra_observation()["channels"], serde_json::json!([true, true]));
    hub.reset();
    assert_eq!(hub.ultra_observation()["channels"], serde_json::json!([false, false]), "a stopped phone session cannot retain its Ultra channels");
    let next = Probe::new();
    hub.set_ultra_probe(Some(&next));
    tasks.abort_all();
    tokio::time::timeout(Duration::from_secs(2), async { while let Some(result) = tasks.join_next().await { assert!(result.unwrap_err().is_cancelled()); } }).await.unwrap();
    assert!(probe.outgoing.lock().unwrap().is_empty(), "cancelled channels must be released even while the phone sockets remain open");
    assert_eq!(hub.ultra_observation()["channels"], serde_json::json!([false, false]), "late cleanup belongs to the old phone session");
    drop(high); drop(normal);
}

#[tokio::test]
async fn normal_only_lane_receives_updates_invalidation_and_recovery(){
    let feed=feed::Feed::new();feed.status(true,true,"reading");let probe=Probe::with_feed(feed.clone());
    let shared=[31;32];let seed=44;let salt=format!("DataStream-Salt{seed}");
    let read=derive(&shared,salt.as_bytes(),b"DataStream-Input-Encryption-Key").unwrap();
    let write=derive(&shared,salt.as_bytes(),b"DataStream-Output-Encryption-Key").unwrap();
    let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let addr=listener.local_addr().unwrap();
    let task=tokio::spawn(stream(listener,addr.ip(),shared,seed,probe.clone(),1));let mut socket=TcpStream::connect(addr).await.unwrap();let mut cipher=ControlCipher::new(read,write);
    socket.write_all(&cipher.encrypt(&package(ASYNC,COMMAND,0,&request("registerRequest",Value::Array(vec![Value::Int(1538)]))).unwrap()).unwrap()).await.unwrap();
    let registered=receive(&mut socket,&mut cipher).await;assert_eq!(opened(&registered[32..]).get("command").and_then(Value::as_str),Some("registerResponse"));
    for (connected,pct) in [(true,Some(68.0)),(false,None),(true,Some(69.0))]{
        feed.status(true,connected,if connected {"reading"}else{"disconnected"});
        if let Some(value)=pct {let now=feed::now_ms();feed.apply(f50_core::vehicle::Update::Charge{soc_pct:Some(f50_core::vehicle::Reading{value,received_ms:now,sample_received_ms:now,sampled_ms:Some(now),sampled_time_trusted:true}),range_km:None});}
        let update=receive(&mut socket,&mut cipher).await;
        let expected=pct.map_or(Value::Null,|n|Value::Int(n as i64));
        assert!(opened(&update[32..]).get("values").unwrap().as_dict().unwrap().contains(&(Value::Int(1538),expected)));
    }
    task.abort();assert!(task.await.unwrap_err().is_cancelled());
}
