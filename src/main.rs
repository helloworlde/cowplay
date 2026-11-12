use cowplay::{
    bluetooth::Manager,
    platform::f50::{self as board, Board},
};
use std::{path::Path, time::Duration};
#[tokio::main(worker_threads = 2)]
async fn main() -> cowplay::RuntimeResult<()> {
    let (stop, mut signals) = tokio::sync::watch::channel(false);
    // Install handlers before synchronous board initialization; procd uses SIGTERM.
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::spawn(async move {
        tokio::select! {_=term.recv()=>{},_=tokio::signal::ctrl_c()=>{}}
        let _ = stop.send(true);
    });
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str){
        Some("probe")=>{probe(args.get(2).map(String::as_str))?;},
        #[cfg(feature = "tesla")]
        Some("tesla-keygen")=>{
            let path=Path::new(args.get(2).ok_or("usage: tesla-keygen NEW_PRIVATE_KEY_FILE")?);
            let public=cowplay::tesla_ble::worker::keygen(path)?;
            println!("{}",serde_json::json!({"public_key":public.iter().map(|b|format!("{b:02x}")).collect::<String>(),"note":"接收端公钥，必须由车主现场授权；私钥留在设备私有目录"}));
        },
        #[cfg(feature = "tesla")]
        Some("tesla-pair")=>{
            let config_path=args.get(2).ok_or("usage: tesla-pair TESLA_CONFIG.json [SERVICE_CONFIG.json]")?;
            let config = cowplay::tesla_ble::management::load_monitor_config(Path::new(config_path))?;
            if let Some(service) = args.get(3) {
                let service: cowplay::service::Config = serde_json::from_slice(&f50_server::settings::read(service,32768)?)?;
                cowplay::tesla_ble::worker::enroll_on(config,signals,service.bluetooth.bus(),service.bluetooth.adapter()).await?;
            } else { cowplay::tesla_ble::worker::enroll(config,signals).await?; }
        },
        Some("verify-image")=>{
            let path=args.get(2).ok_or("image path required")?;
            let arch=args.get(3).map(String::as_str).unwrap_or("arm64");
            cowplay::image::validate_arch(&f50_server::settings::read(path,32*1024*1024)?,arch)?;
            println!("STATIC_{}_ELF_VERIFIED",if arch=="arm64" {"AARCH64"}else{"X86_64"});
        },
        Some("import-dashplay")=>{cowplay::import::from_dashplay(Path::new(args.get(2).ok_or("source required")?),Path::new(args.get(3).ok_or("new destination required")?))?;},
        Some("migrate-state")=>{cowplay::import::migrate_state(Path::new(args.get(2).ok_or("legacy state dir required")?),Path::new(args.get(3).ok_or("new state dir required")?))?;},
        Some("clients-compat")=>{
            let state=Path::new(args.get(2).ok_or("clients-compat STATE_DIR NEW_PRIVATE_OUTPUT required")?);
            let output=Path::new(args.get(3).ok_or("new private output required")?);
            println!("{}",cowplay::clients_compat::prepare(state,output)?);
        },
        Some("ctl")=>{
            let method=args.get(2).map(String::as_str).unwrap_or("");
            match cowplay::ctl::client(method,Path::new(args.get(3).map(String::as_str).unwrap_or(cowplay::ctl::SOCKET))){
                Ok(body)=>println!("{body}"),
                Err(e)=>{println!("{}",serde_json::json!({"error":e}));std::process::exit(1)},
            }
        },
        Some("serve")=>{cowplay::service::serve(args.get(2).ok_or("serve CONFIG.json required")?,signals).await?;},
        Some("network")=>{
            use std::io::Read;
            let mut bytes=Vec::new();std::io::stdin().take(16385).read_to_end(&mut bytes)?;
            let result=if bytes.len()>16384{Err("网络请求过大".into())}else{
                let input=serde_json::from_slice(&bytes).unwrap_or_else(|_|serde_json::json!({}));
                cowplay::network::cli(args.get(2).map(String::as_str).unwrap_or(""),&input).await
            };
            match result {Ok(value)=>println!("{value}"),Err(error)=>{println!("{}",serde_json::json!({"error":error}));std::process::exit(1)}}
        },
        Some("network-worker")=>{cowplay::network::worker(args.get(2).ok_or("network worker id required")?,args.get(3).is_some_and(|s|s=="recover")).await?;},
        Some("update-bootstrap")=>{cowplay::updater::bootstrap()?; println!("发布来源与公钥已就绪");},
        Some("update")=>{
            use std::io::Read;
            let mut bytes=Vec::new();std::io::stdin().take(4097).read_to_end(&mut bytes)?;
            let result=if bytes.len()>4096{Err("更新请求过大".into())}else{
                let input=if bytes.is_empty(){Ok(serde_json::json!({}))}else{serde_json::from_slice(&bytes).map_err(|_|"更新请求不是 JSON".to_owned())};
                match input {Ok(input)=>cowplay::updater::cli(args.get(2).map(String::as_str).unwrap_or("status"),&input).await,Err(e)=>Err(e)}
            };
            match result {Ok(value)=>println!("{value}"),Err(error)=>{println!("{}",serde_json::json!({"error":error}));std::process::exit(1)}}
        },
        Some("update-worker")=>{cowplay::updater::worker(args.get(2).ok_or("update job id required")?).await?;},
        Some("bluetooth-check")=>{
            let state=args.get(2).ok_or("usage: bluetooth-check STATE_DIR EXISTING_BDADDR_FILE [SECONDS]")?;
            let address=args.get(3).ok_or("existing Bluetooth address file required")?;
            let seconds=args.get(4).map(|s|s.parse::<u64>()).transpose()?.unwrap_or(10);
            if !(1..=60).contains(&seconds){return Err("check duration must be 1..60 seconds".into())}
            let mut board=Board::start(Path::new(state),Path::new(address),signals.clone())?;
            let phone_file=Path::new(state).join("phone");
            let (manager,_channels)=tokio::select!{_=signals.changed()=>return Ok(()),r=Manager::register(board::BUS,&phone_file)=>r?};
            println!("BLUEZ_AGENT_AND_PROFILE_READY; pairing closed; CarPlay not verified");
            let until=tokio::time::Instant::now()+Duration::from_secs(seconds);
            loop {if *signals.borrow(){break}tokio::select!{_=signals.changed()=>break,_=tokio::time::sleep_until(until)=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{board.check()?;tokio::select!{_=signals.changed()=>break,r=manager.poll()=>{r?;}}}}}
            manager.stop().await;
            drop(manager);drop(board);
            println!("BLUETOOTH_CHECK_STOPPED; owned children released");
        },
        _=>return Err("usage: cowplay probe | verify-image IMAGE [arm64|x86_64] | import-dashplay SOURCE NEW_DESTINATION | migrate-state OLD_DIR NEW_DIR | serve CONFIG.json | ctl METHOD | update check|status|apply|prepare|acknowledge | bluetooth-check STATE_DIR EXISTING_BDADDR_FILE [SECONDS]".into()),
    }
    Ok(())
}
fn probe(config_path: Option<&str>) -> cowplay::RuntimeResult<()> {
    let f50 = if let Some(path) = config_path {
        let config: cowplay::service::Config = serde_json::from_slice(&f50_server::settings::read(path, 32768)?)?;
        let adapter = config.bluetooth.adapter();
        println!("bluetooth_backend={}", if config.bluetooth.is_f50() { "f50" } else { "system" });
        println!("adapter=hci{adapter}");
        println!("adapter_present={}", Path::new(&format!("/sys/class/bluetooth/hci{adapter}")).exists());
        println!("host_management={}", if config.host().is_openwrt() { "openwrt" } else { "linux" });
        config.bluetooth.is_f50()
    } else { true };
    let openwrt = Path::new("/etc/openwrt_release").is_file();
    println!("openwrt={openwrt}");
    println!("os={} arch={}", std::env::consts::OS, std::env::consts::ARCH);
    println!("system_bluez_bus={}", Path::new("/run/dbus/system_bus_socket").exists());
    for (label, path) in [
        ("openwrt_release", "/etc/openwrt_release"),
        ("package_arch", "/etc/apk/arch"),
        ("kernel", "/proc/sys/kernel/osrelease"),
    ] {
        println!(
            "{label}={}",
            std::fs::read_to_string(path)
                .map(|s| s.trim().to_owned())
                .unwrap_or("unavailable".into())
        );
    }
    for (label, path) in [
        ("apk", "/usr/bin/apk"),
        ("mu300_bt_init", "/opt/mu300/bin/mu300-bt-init"),
        ("bluetooth_tty", "/dev/ttyBT0"),
        ("hci0", "/sys/class/bluetooth/hci0"),
        ("private_bluez", "/usr/lib/cowplay/bluez/usr/bin/bluetoothd"),
    ] {
        if !f50 && matches!(label, "mu300_bt_init" | "bluetooth_tty" | "hci0" | "private_bluez") { continue; }
        println!("{label}={}", Path::new(path).exists());
    }
    println!("carplay_session=not_verified");
    Ok(())
}
