use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the isolated Linux Chrome container, Node and NSS test CA tools"]
async fn browser_uses_real_tesla_api_persistence_and_pairing_lifecycle() {
    let (phone, _bluez, _, dir) = crate::bluetooth::recovery_tests::admin_fixture().await;
    let address = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let origin = format!("https://{address}");
    let (hub, _control) = f50_server::Hub::new("123456".into(), origin.clone());
    let mut ca = rcgen::CertificateParams::new(vec!["CowPlay Synthetic CA".into()]).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca.distinguished_name.push(rcgen::DnType::CommonName, "CowPlay Synthetic Test CA");
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca_certificate = ca.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::from_params(&ca, ca_key);
    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = rcgen::CertificateParams::new(vec!["127.0.0.1".into()]).unwrap().signed_by(&key, &issuer).unwrap();
    let ca_file = dir.path().join("synthetic-ca.pem");
    let certificate_file = dir.path().join("synthetic-server.pem");
    let key_file = dir.path().join("synthetic-server.key");
    std::fs::write(&ca_file, ca_certificate.pem()).unwrap();
    std::fs::write(&certificate_file, format!("{}{}", certificate.pem(), ca_certificate.pem())).unwrap();
    f50_server::settings::write_private(&key_file, key.serialize_pem().as_bytes()).unwrap();
    let admin = crate::admin::Admin::new(phone, hub.clone(), origin.clone(), dir.path().join("cowplay.uci"), dir.path().join("config"), dir.path().to_owned(), certificate_file.display().to_string(), key_file.display().to_string(), address, "127.0.0.1:7000".parse().unwrap(), None, (1254, 784)).unwrap();
    let (stop, stopped) = watch::channel(false);
    let (outcomes, _) = watch::channel(0u8);
    let starts = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let enrollments = Arc::new(AtomicUsize::new(0));
    let authorized = Arc::new(AtomicBool::new(false));
    let cache_generation = Arc::new(AtomicUsize::new(1));
    let cache_requests = Arc::new(AtomicUsize::new(0));
    let actor = {
        let (starts, active, closes, enrollments, authorized, outcomes) = (starts.clone(), active.clone(), closes.clone(), enrollments.clone(), authorized.clone(), outcomes.clone());
        tokio::spawn(admin.tesla.clone().run_with(stopped, move |operation, config, feed, mut cancelled| {
            let (starts, active, closes, enrollments, authorized) = (starts.clone(), active.clone(), closes.clone(), enrollments.clone(), authorized.clone());
            let mut outcome = outcomes.subscribe();
            Box::pin(async move {
                let (_server, bus, remote) = crate::tesla_ble::gatt::tests::fixture().await;
                let mut link = crate::tesla_ble::transport::Link::using_bus(bus, ":1.42".into(), "S0000000000000000C", &mut cancelled).await?;
                assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                starts.fetch_add(1, Ordering::SeqCst);
                let result: Result<Outcome> = async {
                    match operation {
                        Operation::Pair | Operation::ClimatePair => {
                            let identity = Identity::from_pkcs8(&std::fs::read(&config.private_key)?)?;
                            let permission = if operation == Operation::Pair { EnrollmentPermission::VehicleMonitor } else { EnrollmentPermission::Driver };
                            let request = identity.enrollment_request_for(permission);
                            link.send(&request, &mut cancelled).await?;
                            assert_eq!(remote.writes.lock().unwrap().concat(), crate::tesla_ble::wire::Frames::chunks(&request, 20)?.concat());
                            enrollments.fetch_add(1, Ordering::SeqCst);
                            tokio::select! {
                                _ = cancelled.changed() => Err("synthetic vehicle confirmation cancelled".into()),
                                _ = outcome.changed() => if *outcome.borrow() == 1 { authorized.store(true, Ordering::SeqCst); Ok(Outcome::Done) } else { Err("synthetic vehicle declined authorization".into()) },
                            }
                        }
                        Operation::Poll => {
                            if !authorized.load(Ordering::SeqCst) { feed.status(true, false, "not_authorized"); return Err("synthetic vehicle requires enrollment".into()); }
                            feed.status(true, true, "reading");
                            let now = now_ms();
                            feed.apply(f50_core::vehicle::Update::Charge { soc_pct: Some(f50_core::vehicle::Reading { value: 68.0, received_ms: now, sample_received_ms: now, sampled_ms: Some(now), sampled_time_trusted: true }), range_km: None });
                            if !*cancelled.borrow() { let _ = cancelled.changed().await; }
                            Ok(Outcome::Done)
                        }
                        Operation::ClimateQuery | Operation::ClimateCommand(_) => Ok(Outcome::Climate(Some(ClimateState { hvac_on: Some(matches!(operation, Operation::ClimateCommand(_))), inside_temp_celsius: Some(24.0), outside_temp_celsius: Some(28.0), driver_temp_celsius: Some(21.0), passenger_temp_celsius: Some(22.0), min_temp_celsius: Some(15.0), max_temp_celsius: Some(28.0), sampled_ms: Some(now_ms()) }))),
                    }
                }.await;
                let cleanup = link.close().await;
                assert_eq!(remote.disconnects.load(Ordering::SeqCst), 1);
                assert_eq!(remote.unsubscribes.load(Ordering::SeqCst), 1);
                active.fetch_sub(1, Ordering::SeqCst);closes.fetch_add(1, Ordering::SeqCst);
                cleanup?;result
            })
        }))
    };
    let fixture = {
        let (active, starts, closes, enrollments, hub, cache_generation, cache_requests) = (active.clone(), starts.clone(), closes.clone(), enrollments.clone(), hub.clone(), cache_generation.clone(), cache_requests.clone());
        axum::Router::new().route("/__tesla_fixture", axum::routing::post(move |axum::Json(input): axum::Json<serde_json::Value>| {
            let (active, starts, closes, enrollments, outcomes, hub, cache_generation, cache_requests) = (active.clone(), starts.clone(), closes.clone(), enrollments.clone(), outcomes.clone(), hub.clone(), cache_generation.clone(), cache_requests.clone());
            async move {
                if let Some(generation) = input["cache_generation"].as_u64() { cache_generation.store(generation as usize, Ordering::SeqCst); }
                if let Some(result) = input["outcome"].as_u64() { outcomes.send(result as u8).unwrap(); }
                if input["video"] == "green" {
                    // One synthetic solid-colour H.264 frame generated with libx264, no audio.
                    let mut config = vec![1, 110, 1, 64, 0, 0, 0, 64, 0, 0, 0];
                    config.extend_from_slice(&[1, 66, 192, 10, 255, 225, 0, 22, 103, 66, 192, 10, 220, 66, 108, 4, 64, 0, 0, 3, 0, 64, 0, 0, 3, 0, 163, 196, 137, 224, 1, 0, 4, 104, 206, 15, 200]);
                    hub.phone_active.store(true, Ordering::Release);
                    hub.phone_verified.store(true, Ordering::Release);
                    hub.set_config(config).unwrap();
                    for index in 0..5 {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            assert!(hub.publish(f50_core::media::Frame { data: vec![0, 0, 0, 1, 101, 136, 132, 58, 17, 138, 0, 2, 6, 241, 192, 0, 64, 38, 56, 0, 8, 49, 201, 201, 201, 215, 93, 117, 215, 93, 117, 215, 93, 117, 224], pts_us: index * 100_000, key: true }));
                    }
                } else if input["video"] == "stop" {
                    hub.phone_active.store(false, Ordering::Release);
                    hub.phone_verified.store(false, Ordering::Release);
                }

                axum::Json(serde_json::json!({"active":active.load(Ordering::SeqCst),"starts":starts.load(Ordering::SeqCst),"closes":closes.load(Ordering::SeqCst),"enrollments":enrollments.load(Ordering::SeqCst),"cache_requests":cache_requests.load(Ordering::SeqCst)}))
            }
        }))
    };
    let token = "ab".repeat(32);
    let html = std::fs::read_to_string("web/dist/car.html").unwrap();
    let bundle_path = html.split('"').find(|p| p.starts_with("/assets/car-") && p.ends_with(".js")).unwrap();
    let stale_html = html.replace(bundle_path, "/assets/__stale-car-bundle.js");
    let stale_bundle = std::fs::read_to_string(format!("web/dist{bundle_path}")).unwrap().replace(f50_server::build_info::BUILD_ID, "synthetic-old-car-build");
    let cache_probe = axum::Router::new().route("/__cache-probe", axum::routing::get(move || {
        let (generation, requests) = (cache_generation.clone(), cache_requests.clone());
        async move {
            requests.fetch_add(1, Ordering::SeqCst);
            ([("Content-Type", "text/plain"), ("Cache-Control", "public, max-age=31536000, immutable")], generation.load(Ordering::SeqCst).to_string())
        }
    }));
    let cache_model = axum::Router::new()
        .route("/__stale-car", axum::routing::get(move |uri: axum::http::Uri| { let (html, stale_html) = (html.clone(), stale_html.clone()); async move { let fresh = uri.query().is_some_and(|q| q.contains("cowplay_refresh=")); ([("Content-Type", "text/html"), ("Cache-Control", if fresh { "no-store" } else { "public, max-age=31536000, immutable" })], if fresh { html } else { stale_html }) } }))
        .route("/assets/__stale-car-bundle.js", axum::routing::get(move || { let script = stale_bundle.clone(); async move { ([("Content-Type", "text/javascript"), ("Cache-Control", "public, max-age=31536000, immutable")], script) } }));
    let routes = crate::admin::management::router(admin.clone(), Some(Sha256::digest(token.as_bytes()).into()))
        .merge(crate::web::router(admin.manager.clone(), hub.clone()))
        .merge(crate::admin::router(admin.clone()))
        .merge(crate::console::router(admin.clone()))
        .merge(fixture).merge(cache_model).merge(cache_probe);
    let handle = f50_server::Handle::new();
    let server = tokio::spawn(f50_server::run_with_router(f50_server::Config { bind: address, origin: origin.clone(), tls_certificate: certificate_file.display().to_string(), tls_private_key: key_file.display().to_string(), access_token: "123456".into() }, hub, handle.clone(), routes));
    assert_eq!(handle.listening().await, Some(address));
    let trusted = reqwest::Client::builder().add_root_certificate(reqwest::Certificate::from_pem(ca_certificate.pem().as_bytes()).unwrap()).https_only(true).build().unwrap();
    assert_eq!(trusted.get(format!("{origin}/manage")).send().await.unwrap().status(), reqwest::StatusCode::OK);
    let output = tokio::time::timeout(Duration::from_secs(180), tokio::process::Command::new("node").arg("scripts/test-tesla-backend-web.mjs").arg(&origin).arg(&ca_file).env("NODE_EXTRA_CA_CERTS", &ca_file).kill_on_drop(true).output()).await.unwrap().unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let saved = load(&dir.path().join("tesla/config.json"), &dir.path().join("tesla-ble.json")).unwrap().unwrap();
    assert_eq!(saved.vin, "SYNTH000000000001");
    assert!(saved.control_private_key.is_some());
    assert_ne!(saved.control_private_key.as_ref().unwrap(), &saved.private_key);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(dir.path().join("tesla/config.json")).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(Manager::new(dir.path(), Feed::new()).status().public_key_fingerprint, admin.tesla.status().public_key_fingerprint);
    stop.send(true).unwrap();actor.await.unwrap();
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(starts.load(Ordering::SeqCst), closes.load(Ordering::SeqCst));
    assert!(enrollments.load(Ordering::SeqCst) >= 5);
    handle.shutdown();server.await.unwrap().unwrap();
}
