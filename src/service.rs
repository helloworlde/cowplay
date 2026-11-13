use crate::{
    bluetooth::Manager,
    platform::{Bluetooth, Host},
    rfcomm, web,
};
use f50_server::{
    Handle, Hub,
    receiver::Receiver,
    settings::{self, Settings},
};
use std::{path::PathBuf, time::Duration};
pub(crate) struct Hotspot {
    pub ssid: String,
    pub password: zeroize::Zeroizing<String>,
    pub channel: Option<u8>,
    pub security: u8,
}
/// Select only the explicitly configured AP (or a unique matching SSID). Never reconfigure a radio.
pub(crate) fn uci_hotspot(p: &crate::uci::Package, iface: &str, known_ssid: &str) -> Result<Option<Hotspot>, String> {
    let candidates: Vec<_> = p.sections.iter().filter(|s| s.kind == "wifi-iface" && s.get("mode") == Some("ap") && s.get("disabled") != Some("1"))
        .filter(|s| if iface.is_empty() { s.get("ssid") == Some(known_ssid) } else { s.name.as_deref() == Some(iface) || s.get("ifname") == Some(iface) }).collect();
    let [ap] = candidates.as_slice() else {
        return if iface.is_empty() { Ok(None) } else { Err("CowPlay 的无线接口未唯一匹配启用的热点，请在管理页核对".into()) };
    };
    let ssid = ap.get("ssid").ok_or("热点缺少 SSID")?.to_owned();
    let security = crate::handoff_security_code(ap.get("encryption").unwrap_or("none")).ok_or("CowPlay 不支持此热点的加密方式")?;
    let password = zeroize::Zeroizing::new(ap.get("key").unwrap_or("").to_owned());
    if !f50_core::validation::hotspot(&ssid, &password) { return Err("热点 SSID 或密码无效".into()); }
    let channel = ap.get("device").and_then(|d| p.get(d, "channel")).and_then(|c| c.parse::<u8>().ok()).filter(|c| *c > 0);
    Ok(Some(Hotspot {ssid, password, channel, security}))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub state_dir: PathBuf,
    #[serde(default)]
    pub bluetooth_address_file: Option<PathBuf>,
    #[serde(default)]
    pub bluetooth: Bluetooth,
    #[serde(default)]
    pub host: Option<Host>,
    #[serde(default)]
    pub policy: Option<crate::policy::Store>,
    #[serde(default)]
    pub management_token_file: Option<PathBuf>,
    pub receiver: Settings,
    /// UCI policy file edited in LuCI (default /etc/config/cowplay).
    #[serde(default)]
    pub uci_config: Option<PathBuf>,
    /// LuCI control socket (default /var/run/cowplay.sock).
    #[serde(default)]
    pub ctl_socket: Option<PathBuf>,
}
impl Config {
    pub fn host(&self) -> Host {
        self.host.unwrap_or(if self.bluetooth.is_f50() { Host::Openwrt } else { Host::Linux })
    }
    pub fn policy_store(&self) -> Result<crate::policy::Store, String> {
        if self.policy.is_some() && self.uci_config.is_some() { return Err("choose policy or uci_config, not both".into()); }
        Ok(self.policy.clone().unwrap_or_else(|| {
            if self.host().is_openwrt() || self.uci_config.is_some() {
                crate::policy::Store::Uci { path: self.uci_config.clone().unwrap_or_else(|| PathBuf::from(crate::policy::DEFAULT_PATH)) }
            } else {
                crate::policy::Store::Json { path: self.state_dir.join("policy.json") }
            }
        }))
    }
}
pub async fn serve(
    path: &str,
    mut signals: tokio::sync::watch::Receiver<bool>,
) -> crate::RuntimeResult<()> {
    let mut config: Config = serde_json::from_slice(&settings::read(path, 32768)?)?;
    let openwrt = config.host().is_openwrt();
    let management_secret = crate::admin::management::load_token(config.management_token_file.as_deref())?;
    config.receiver.name = crate::receiver_name::for_device(crate::receiver_name::local_device_name());
    let receiver_name = config.receiver.name.clone();
    let store = config.policy_store()?;
    // LuCI owns the public address: a domain/port in UCI overrides config.json.
    let policy = if store.path().exists() {
        let pkg = store.read()?;
        crate::policy::Policy::validate_display(&pkg)?;
        crate::policy::Policy::from_uci(&pkg)
    } else { crate::policy::Policy::default() };
    if openwrt || store.path().exists() || config.uci_config.is_some() {
        let web = &mut config.receiver.web;
        web.bind.set_port(policy.port);
        let mut origin = reqwest::Url::parse(&web.origin)?;
        if !policy.domain.is_empty() { origin.set_host(Some(&policy.domain))?; }
        origin.set_port(Some(policy.port)).map_err(|_|"HTTPS 端口无效")?;
        web.origin = origin.origin().ascii_serialization();
    }
    let mut wifi = config.receiver.wifi.as_ref().map(|w| (w.ssid.clone(), w.channel));
    let display = (config.receiver.width, config.receiver.height);
    // Validate user identity/TLS before taking ownership of the controller.
    let running_mfi_key = std::path::PathBuf::from(&config.receiver.mfi_pkcs8_file);
    let mut prepared = config.receiver.prepare()?;
    if openwrt {
    if let Some((ssid, previous_channel)) = &wifi {
        if let Ok(pkg) = crate::uci::Package::read(std::path::Path::new("/etc/config/wireless")) {
            if let Some(hotspot) = uci_hotspot(&pkg, &policy.wifi_iface, ssid)? {
                if let Some(credentials) = std::sync::Arc::get_mut(&mut prepared.credentials) {
                    if let Some(endpoint) = credentials.endpoint.as_mut().and_then(std::sync::Arc::get_mut) {
                        endpoint.ssid = hotspot.ssid.clone(); endpoint.password = hotspot.password;
                        endpoint.channel = hotspot.channel.unwrap_or(*previous_channel); endpoint.security = hotspot.security;
                        wifi = Some((endpoint.ssid.clone(), endpoint.channel));
                    }
                }
            }
        }
    }
    }
    f50_server::validate_tls_files(&prepared.web).await?;
    let endpoint = prepared
        .credentials
        .endpoint
        .clone()
        .ok_or("configured Wi-Fi endpoint required for Bluetooth handoff")?;
    let identity = prepared.credentials.mfi.clone();
    let listener = tokio::net::TcpListener::bind(prepared.control_bind).await?;
    let mut board = config.bluetooth.start(&config.state_dir, config.bluetooth_address_file.as_deref(), signals.clone())?;
    let phone_file = config.state_dir.join("phone");
    let (manager, mut channels) = tokio::select! {
        _=signals.changed()=>return Ok(()),
        result=Manager::register_named_on(config.bluetooth.bus(),&phone_file,config.bluetooth.adapter(),config.bluetooth.is_f50(),&receiver_name)=>result?,
    };
    prepared.pairings.initialize(manager.selected_phone().as_deref())?;
    let (hub, commands) = Hub::with_phones(
        prepared.web.access_token.clone(),
        prepared.web.origin.clone(),
        prepared.pairings.clone(),
    );
    let (stop, rx) = tokio::sync::watch::channel(false);
    hub.clients.load(config.state_dir.clone())?;
    let admin = crate::admin::Admin::with_store(
        manager.clone(),
        hub.clone(),
        prepared.web.origin.clone(),
        store,
        PathBuf::from(path),
        config.state_dir.clone(),
        prepared.web.tls_certificate.clone(),
        prepared.web.tls_private_key.clone(),
        prepared.web.bind,
        prepared.control_bind,
        wifi,
        display,
        openwrt,
    )?;
    {
        use der::Encode;
        let cert = f50_core::mfi::public_certificate(identity.certificate()).map_err(|e|format!("运行时 CarPlay 公开证书解析失败：{e:?}"))?.to_der()?;
        *admin.public_mfi.lock().unwrap() = Some(vec![cert]);
        *admin.running_mfi_key.lock().unwrap() = Some(running_mfi_key);
    }
    let management = crate::admin::management::router(admin.clone(), management_secret);
    let vehicle_expiry = tokio::spawn(expire_vehicle_feed(hub.vehicle.clone(), rx.clone()));
    #[cfg(feature = "tesla")]
    let vehicle_worker=tokio::spawn(admin.tesla.clone().run(config.bluetooth.bus().to_owned(),config.bluetooth.adapter(),rx.clone()));
    *admin.active_wifi_password.lock().unwrap() = Some(endpoint.password.clone());
    let routes = web::router(manager.clone(), hub.clone())
        .merge(crate::admin::router(admin.clone()))
        .merge(crate::console::router(admin.clone()))
        .merge(management);
    let routes = if openwrt { routes } else {
        routes.merge(crate::admin::management::entry(config.management_token_file.is_some()))
    };
    let ctl = {
        let admin = admin.clone();
        let socket = config.ctl_socket.clone().unwrap_or_else(|| if openwrt { PathBuf::from(crate::ctl::SOCKET) } else { config.state_dir.join("control.sock") });
        tokio::spawn(async move {
            if let Err(e) = crate::ctl::serve(admin, &socket).await {
                f50_server::diag_log!(Warn, "web", "LuCI control socket unavailable: {e}");
            }
        })
    };
    let redirect = admin.policy().redirect_http.then(|| {
        let bind = std::net::SocketAddr::new(prepared.web.bind.ip(), 18080);
        let origin = prepared.web.origin.clone();
        tokio::spawn(async move {
            let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
                let target = format!("{origin}{}", uri.path_and_query().map(|p| p.as_str()).unwrap_or("/"));
                async move { axum::response::Redirect::permanent(&target) }
            });
            match tokio::net::TcpListener::bind(bind).await {
                Ok(l) => { let _ = axum::serve(l, app).await; }
                Err(e) => f50_server::diag_log!(Warn, "web", "HTTP redirect on {bind} unavailable: {e}"),
            }
        })
    });
    let sampler = {
        let admin = admin.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(1));
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                every.tick().await;
                admin.sample().await;
            }
        })
    };
    let ddns = tokio::spawn(admin.clone().ddns_loop());
    let automatic = tokio::spawn(admin.clone().run_standby(rx.clone()));
    f50_server::diag_log!(Info, "web", "service started build={}", f50_server::build_info::BUILD_ID);
    let receiver = Receiver {
        credentials: prepared.credentials,
        hub: hub.clone(),
        pairing_window: prepared.pairing_window,
    };
    let handle = Handle::new();
    let mut receiver = tokio::spawn(receiver.run(listener, commands, rx));
    let server = f50_server::run_with_router(prepared.web, hub.clone(), handle.clone(), routes);
    tokio::pin!(server);
    let mut links = tokio::task::JoinSet::new();
    let mut server_finished = false;
    let result = loop {
        if *signals.borrow() {
            break Ok(());
        }
        tokio::select! {
            _=signals.changed()=>break Ok(()),
            r=&mut server=>{server_finished=true;break r},
            r=&mut receiver=>break match r {Ok(Ok(()))=>Err("network receiver exited".into()),Ok(Err(e))=>Err(e.into()),Err(e)=>Err(e.into())},
            Some(channel)=channels.recv()=>{
                let selected=hub.phones.snapshot();
                if !hub.has_phone_session() && selected.epoch==channel.selection && selected.device.as_deref()==Some(channel.device.as_str()) { admin.prepare_display(); }
                let m=manager.clone();let e=endpoint.clone();let i=identity.clone();let h=hub.clone();links.spawn(async move{rfcomm::run(channel,m,e,i,h).await});
            },
            Some(result)=links.join_next(),if !links.is_empty()=>{if !matches!(result,Ok(Ok(()))){f50_server::diag_echo!(Warn, "bt", "RFCOMM_SESSION_ENDED_WITH_ERROR; reconnect available");}},
            _=tokio::time::sleep(Duration::from_secs(1))=>{
                if let Some(board) = board.as_mut() { if let Err(e)=board.check(){break Err(e)} }
                // Read the selection before polling: a cancel racing the observed bond must win.
                let epoch=hub.phones.epoch();
                let polled=tokio::select!{_=signals.changed()=>break Ok(()),r=manager.poll()=>r};
                if let Err(e)=polled{break Err(e)}
                if let Some(device)=manager.take_new_pairing() {
                    let (m,h)=(manager.clone(),hub.clone());
                    tokio::spawn(async move{crate::phone_flow::connect_new_pairing(&m,&h,&device,epoch).await});
                }
            }
        }
    };
    // Share the existing 8s phone + 3s HTTP budget; leave 3s for owned-child cleanup
    // and a margin before procd's 15s SIGKILL. Tesla starts stopping concurrently.
    let shutdown_deadline = tokio::time::Instant::now() + Duration::from_secs(14);
    #[cfg(feature = "tesla")]
    let vehicle_deadline = shutdown_deadline - Duration::from_secs(3);
    let _ = stop.send(true);
    // Join the coordinator before stopping BlueZ; no automatic work survives service shutdown.
    let _ = automatic.await;
    vehicle_expiry.abort();
    let _ = vehicle_expiry.await;
    sampler.abort();
    ddns.abort();
    ctl.abort();
    if let Some(r) = redirect {
        r.abort();
    }
    // A callback may have queued an FD just before SIGTERM won the select.
    channels.close();
    while let Ok(channel) = channels.try_recv() {
        let epoch = channel.epoch;
        drop(channel);
        manager.complete(epoch);
    }
    manager.stop().await;
    links.abort_all();
    while links.join_next().await.is_some() {}
    handle.graceful_shutdown(Some(Duration::from_secs(2)));
    // Graceful shutdown must continue polling the pinned server to drain its sockets.
    if !server_finished {
        let _ = tokio::time::timeout(Duration::from_secs(3), &mut server).await;
    }
    if !receiver.is_finished() {
        receiver.abort();
        let _ = receiver.await;
    }
    hub.reset();
    #[cfg(feature = "tesla")]
    finish_vehicle_worker(vehicle_worker, vehicle_deadline).await;
    drop(manager);
    drop(board);
    f50_server::diag_log!(Info, "web", "service stopped; draining persistent diagnostics");
    let _ = f50_server::diag::diag().shutdown_sink(shutdown_deadline).await;
    result
}

async fn expire_vehicle_feed(feed: std::sync::Arc<f50_server::vehicle::Feed>, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut every = tokio::time::interval(Duration::from_secs(1));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while !*stop.borrow() {
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            _ = every.tick() => feed.tick(),
        }
    }
}

#[cfg(feature = "tesla")]
async fn finish_vehicle_worker(mut worker: tokio::task::JoinHandle<()>, deadline: tokio::time::Instant) {
    if tokio::time::timeout_at(deadline, &mut worker).await.is_err() {
        worker.abort();
        let _ = worker.await;
        f50_server::diag_log!(Warn,"tesla","Tesla BLE停止超时");
    }
}

#[cfg(test)]
mod ui_policy_tests {
    use super::*;
    #[tokio::test]
    async fn generic_vehicle_expiry_publishes_stale_values_and_stops_without_a_vendor() {
        use f50_core::vehicle::{Reading, Update};
        let feed = f50_server::vehicle::Feed::new();
        feed.status(true, true, "reading");
        let received = f50_server::vehicle::now_ms() - 29_900;
        feed.apply(Update::Charge {
            soc_pct: Some(Reading { value: 68.0, received_ms: received, sample_received_ms: received, sampled_ms: Some(received), sampled_time_trusted: true }),
            range_km: None,
        });
        assert_eq!(feed.snapshot().values.soc_pct, Some(68.0));
        let mut changes = feed.subscribe();
        let (stop, rx) = tokio::sync::watch::channel(false);
        let expiry = tokio::spawn(expire_vehicle_feed(feed.clone(), rx));
        tokio::time::timeout(Duration::from_secs(2), changes.wait_for(|snapshot| snapshot.values.soc_pct.is_none())).await.unwrap().unwrap();
        assert_eq!(feed.snapshot().telemetry.soc_pct.unwrap().value, 68.0);
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_millis(200), expiry).await.unwrap().unwrap();
    }
    #[cfg(feature = "tesla")]
    #[tokio::test]
    async fn stalled_vehicle_worker_cannot_add_a_new_shutdown_wait_after_shared_deadline() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        struct Released(Arc<AtomicBool>);
        impl Drop for Released { fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); } }
        let released=Arc::new(AtomicBool::new(false));
        let flag=released.clone();let (ready,started)=tokio::sync::oneshot::channel();
        let worker=tokio::spawn(async move {let _guard=Released(flag);let _=ready.send(());std::future::pending::<()>().await;});
        started.await.unwrap();
        let deadline=tokio::time::Instant::now()-Duration::from_millis(1);
        tokio::time::timeout(Duration::from_millis(200),finish_vehicle_worker(worker,deadline)).await.expect("Tesla must consume only the remaining shared stop budget");
        assert!(released.load(Ordering::SeqCst),"aborted worker is joined before owned BlueZ cleanup");
    }
    #[test]
    fn wireless_rename_follows_the_explicit_section_and_rejects_ambiguous_aps() {
        let p = crate::uci::Package::parse("config wifi-device 'radio1'\n option channel '149'\nconfig wifi-iface 'car_ap'\n option mode 'ap'\n option device 'radio1'\n option ssid 'New CowPlay'\n option key 'synthetic-only-password'\n option encryption 'psk2+ccmp'\nconfig wifi-iface 'other_ap'\n option mode 'ap'\n option ssid 'New CowPlay'\n option key 'other-test-password'\n option encryption 'psk2'\n").unwrap();
        let hotspot = uci_hotspot(&p, "car_ap", "Old CowPlay").unwrap().unwrap();
        assert_eq!(hotspot.ssid, "New CowPlay");
        assert_eq!(hotspot.channel, Some(149));
        assert_eq!(hotspot.security, 2);
        assert!(uci_hotspot(&p, "", "New CowPlay").unwrap().is_none());
        assert!(uci_hotspot(&p, "missing", "New CowPlay").is_err());
    }
}

#[cfg(test)]
mod platform_tests {
    use super::*;
    fn config() -> serde_json::Value { serde_json::from_str(include_str!("../packaging/config.example.json")).unwrap() }
    #[test]
    fn existing_configuration_keeps_f50_private_runtime_and_uci() {
        let c: Config = serde_json::from_value(config()).unwrap();
        assert!(c.bluetooth.is_f50());
        assert_eq!(c.host(), Host::Openwrt);
        assert_eq!(c.bluetooth.bus(), crate::board::BUS);
        assert_eq!(c.bluetooth.adapter(), 0);
        assert_eq!(c.policy_store().unwrap().path(), std::path::Path::new(crate::policy::DEFAULT_PATH));
    }
    #[test]
    fn linux_configuration_is_explicit_and_does_not_require_f50_identity_file() {
        let mut v = config();
        v["bluetooth"] = serde_json::json!({"kind":"system", "adapter":7});
        v.as_object_mut().unwrap().remove("bluetooth_address_file");
        let mut c: Config = serde_json::from_value(v).unwrap();
        assert!(!c.bluetooth.is_f50());
        assert_eq!(c.host(), Host::Linux);
        assert_eq!(c.bluetooth.adapter(), 7);
        assert_eq!(c.bluetooth.bus(), "unix:path=/run/dbus/system_bus_socket");
        assert!(matches!(c.policy_store().unwrap(), crate::policy::Store::Json { .. }));
        c.policy = Some(crate::policy::Store::Json { path: "/synthetic/policy.json".into() });
        c.uci_config = Some("/synthetic/cowplay.uci".into());
        assert!(c.policy_store().is_err());
    }
    #[test]
    fn system_bluetooth_can_use_openwrt_management_with_uci() {
        let mut v = config();
        v["bluetooth"] = serde_json::json!({"kind":"system", "adapter":7});
        v["host"] = serde_json::json!("openwrt");
        let mut c: Config = serde_json::from_value(v).unwrap();
        assert!(!c.bluetooth.is_f50());
        assert_eq!(c.bluetooth.bus(), "unix:path=/run/dbus/system_bus_socket");
        assert_eq!(c.host(), Host::Openwrt);
        assert!(matches!(c.policy_store().unwrap(), crate::policy::Store::Uci { .. }));
        c.uci_config = Some("/synthetic/cowplay.uci".into());
        assert_eq!(c.policy_store().unwrap().path(), std::path::Path::new("/synthetic/cowplay.uci"));
    }

}
