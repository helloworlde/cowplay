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
        name: "CowPlay", model: "F50", manufacturer: "CowPlay",
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
        assert!(fields["name"].as_string().unwrap().contains("Tesla BLE"));
        assert!(fields.contains_key("vehicleStateProtocolInfo"));
        assert_eq!(fields["displayPanels"].as_array().unwrap().len(), 2);
    } else {
        assert_eq!(actual, ordinary, "explicit disable must preserve ordinary CarPlay info");
    }
}

#[test]
fn live_profile_declares_no_synthetic_climate_or_vehicle_values() {
    let bytes = ordinary_info();
    let changed = Probe::new().augment_info(&bytes).unwrap();
    let changed = bplist::decode(&changed).unwrap();
    let fields = changed.as_dictionary().unwrap();
    assert!(fields["name"].as_string().unwrap().contains("Tesla BLE"));
    assert_eq!(fields["displayPanels"].as_array().unwrap().len(), 2);
    let protocol = fields["vehicleStateProtocolInfo"].as_dictionary().unwrap();
    assert_eq!(protocol["protocolVersion"].as_string(), Some("1.0"));
    let state = Probe::new();
    let state = state.state.lock().unwrap();
    assert!(state.climate().is_none(),"unimplemented controls must not be advertised");
    assert!(bplist::decode(&bytes).unwrap().as_dictionary().unwrap().get("vehicleStateProtocolInfo").is_none());
}

#[tokio::test]
async fn encrypted_phone_reads_read_only_state_and_receives_live_cross_lane_updates() {
    let feed=feed::Feed::new();
    feed.status(true,true,"reading");
    let probe = Probe::with_feed(feed.clone());
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
    tasks.abort_all();
    tokio::time::timeout(Duration::from_secs(2), async { while let Some(result) = tasks.join_next().await { assert!(result.unwrap_err().is_cancelled()); } }).await.unwrap();
    assert!(probe.outgoing.lock().unwrap().is_empty(), "cancelled channels must be released even while the phone sockets remain open");
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
