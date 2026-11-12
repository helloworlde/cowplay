use super::*;
use crate::tesla_ble::transport::Link;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Observed {
    started: AtomicUsize,
    active: AtomicUsize,
    closed: AtomicUsize,
}
fn runner(
    observed: Arc<Observed>,
) -> impl Fn(Operation, Config, Arc<Feed>, watch::Receiver<bool>) -> Job {
    move |_, _, feed, mut stop| {
        let observed = observed.clone();
        Box::pin(async move {
            let (_server, bus, remote) = crate::tesla_ble::gatt::tests::fixture().await;
            let mut link =
                Link::using_bus(bus, ":1.42".into(), "S0000000000000000C", &mut stop).await?;
            assert_eq!(
                observed.active.fetch_add(1, Ordering::SeqCst),
                0,
                "only one target GATT owner"
            );
            observed.started.fetch_add(1, Ordering::SeqCst);
            if !*stop.borrow() {
                let _ = stop.changed().await;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            feed.status(true, true, "reading");
            let now = now_ms();
            feed.apply(f50_core::vehicle::Update::Charge {
                soc_pct: Some(f50_core::vehicle::Reading {
                    value: 77.0,
                    received_ms: now,
                    sample_received_ms: now,
                    sampled_ms: Some(now),
                    sampled_time_trusted: true,
                }),
                range_km: None,
            });
            link.close().await?;
            assert_eq!(remote.disconnects.load(Ordering::SeqCst), 1);
            assert_eq!(remote.unsubscribes.load(Ordering::SeqCst), 1);
            observed.active.fetch_sub(1, Ordering::SeqCst);
            observed.closed.fetch_add(1, Ordering::SeqCst);
            Ok(Outcome::Done)
        })
    }
}
async fn started(observed: &Observed, expected: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while observed.started.load(Ordering::SeqCst) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn configure(vin: &str, enabled: bool) -> Command {
    Command::Configure {
        vin: Some(vin.into()),
        enabled,
    }
}

#[tokio::test]
async fn pair_cancel_waits_for_gatt_cleanup_and_disconnect_requires_explicit_connect() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path(), Feed::new());
    let observed = Arc::new(Observed::default());
    let (stop, rx) = watch::channel(false);
    let actor = tokio::spawn(manager.clone().run_with(rx, runner(observed.clone())));
    manager
        .command(configure("5YJ30123456789ABC", true))
        .await
        .unwrap();
    started(&observed, 1).await;
    let pair = manager.command(Command::Pair).await.unwrap();
    assert_eq!(pair.pairing, "pairing");
    assert!(pair.pairing_deadline_ms.unwrap() > now_ms());
    assert_eq!(observed.closed.load(Ordering::SeqCst), 1);
    started(&observed, 2).await;
    let cancelled = manager.command(Command::Cancel).await.unwrap();
    assert_eq!(cancelled.pairing, "cancelled");
    assert_eq!(cancelled.phase, "pairing_cancelled");
    assert_eq!(observed.closed.load(Ordering::SeqCst), 2);
    assert_eq!(observed.active.load(Ordering::SeqCst), 0);
    assert!(!cancelled.connected);
    assert_eq!(cancelled.vehicle.values.soc_pct, None);
    manager.feed.request_retry();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(observed.started.load(Ordering::SeqCst), 2);
    manager.command(Command::Connect).await.unwrap();
    started(&observed, 3).await;
    manager.command(Command::Disconnect).await.unwrap();
    assert_eq!(observed.closed.load(Ordering::SeqCst), 3);
    manager.feed.request_retry();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(observed.started.load(Ordering::SeqCst), 3);
    stop.send(true).unwrap();
    actor.await.unwrap();
}

#[tokio::test]
async fn new_configuration_isolates_late_reads_and_preserves_keys_and_private_atomic_state() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path(), Feed::new());
    let observed = Arc::new(Observed::default());
    let (stop, rx) = watch::channel(false);
    let actor = tokio::spawn(manager.clone().run_with(rx, runner(observed.clone())));
    manager
        .command(configure("5YJ30123456789ABC", true))
        .await
        .unwrap();
    started(&observed, 1).await;
    let old = manager
        .state
        .lock()
        .unwrap()
        .saved
        .clone()
        .unwrap()
        .private_key;
    let old_key = std::fs::read(&old).unwrap();
    let configured = manager
        .command(configure("5YJ30123456789ABD", false))
        .await
        .unwrap();
    assert!(!configured.enabled);
    assert_eq!(configured.phase, "disabled");
    assert_eq!(
        configured.vehicle.telemetry,
        f50_core::vehicle::Telemetry::default()
    );
    assert_eq!(configured.vehicle.values.soc_pct, None);
    assert_eq!(observed.active.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(&old).unwrap(), old_key);
    assert_eq!(
        std::fs::metadata(manager.directory.join("config.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&manager.directory)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let rendered = serde_json::to_string(&configured).unwrap();
    assert!(!rendered.contains("5YJ30123456789ABD"));
    assert!(!rendered.contains(&old.to_string_lossy().to_string()));
    assert!(!rendered.contains("private_key"));
    assert!(configured.key_ready);
    let reloaded = Manager::new(dir.path(), Feed::new()).status();
    assert_eq!(reloaded.vin_masked, configured.vin_masked);
    assert!(!reloaded.enabled);
    manager
        .command(Command::Configure {
            vin: None,
            enabled: true,
        })
        .await
        .unwrap();
    started(&observed, 2).await;
    stop.send(true).unwrap();
    actor.await.unwrap();
    assert_eq!(observed.closed.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn legacy_identity_is_reused_and_configuration_never_accepts_a_key_path() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("legacy-monitor.pk8");
    worker::keygen(&key).unwrap();
    let original = std::fs::read(&key).unwrap();
    f50_server::settings::write_private(
        &dir.path().join("tesla-ble.json"),
        &serde_json::to_vec(&serde_json::json!({"vin":"5YJ30123456789ABC","private_key":key}))
            .unwrap(),
    )
    .unwrap();
    let manager = Manager::new(dir.path(), Feed::new());
    assert!(manager.status().key_ready);
    assert!(manager.status().enabled);
    let (stop, rx) = watch::channel(false);
    let actor = tokio::spawn(manager.clone().run_with(rx, |_, _, _, mut stop| {
        Box::pin(async move {
            let _ = stop.changed().await;
            Ok(Outcome::Done)
        })
    }));
    manager
        .command(Command::Configure {
            vin: None,
            enabled: false,
        })
        .await
        .unwrap();
    assert_eq!(
        manager
            .state
            .lock()
            .unwrap()
            .saved
            .as_ref()
            .unwrap()
            .private_key,
        key
    );
    assert_eq!(std::fs::read(&key).unwrap(), original);
    assert!(manager.command(configure("INVALID", true)).await.is_err());
    assert!(!manager.status().enabled);
    stop.send(true).unwrap();
    actor.await.unwrap();
}

#[tokio::test]
async fn uncertain_cleanup_blocks_pairing_and_followup_connections() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path(), Feed::new());
    let (stop, rx) = watch::channel(false);
    let actor = tokio::spawn(manager.clone().run_with(rx, |_, _, feed, mut stop| {
        Box::pin(async move {
            let _ = stop.changed().await;
            feed.status(true, false, "cleanup_failed");
            Err("synthetic uncertain disconnect".into())
        })
    }));
    manager
        .command(configure("5YJ30123456789ABC", true))
        .await
        .unwrap();
    assert!(manager.command(Command::Pair).await.is_err());
    assert_eq!(manager.status().phase, "cleanup_failed");
    assert!(manager.command(Command::Connect).await.is_err());
    stop.send(true).unwrap();
    actor.await.unwrap();
}

async fn climate_ready(manager: &Manager) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while manager.status().climate.phase != "ready" {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn native_commands_recheck_revocation_after_cleanup_and_cancel_without_retries() {
    use std::sync::atomic::AtomicBool;
    let dir=tempfile::tempdir().unwrap();let feed=Feed::new();let manager=Manager::new(dir.path(),feed.clone());
    let commands=Arc::new(AtomicUsize::new(0));let closed=Arc::new(AtomicUsize::new(0));
    let revoke_poll=Arc::new(AtomicBool::new(false));let (grant,_)=watch::channel(None);
    let (stop,rx)=watch::channel(false);
    let actor=tokio::spawn(manager.clone().run_with(rx,{
        let commands=commands.clone();let closed=closed.clone();let revoke_poll=revoke_poll.clone();let grant=grant.clone();
        move |operation,_,feed,mut cancel| {
            let commands=commands.clone();let closed=closed.clone();let revoke_poll=revoke_poll.clone();let grant=grant.clone();
            Box::pin(async move { match operation {
                Operation::Poll=>{let _=cancel.changed().await;if revoke_poll.swap(false,Ordering::SeqCst){grant.send_replace(None);grant.send_replace(Some(feed.climate().context));}Ok(Outcome::Done)},
                Operation::ClimatePair=>Ok(Outcome::Done),
                Operation::ClimateQuery=>Ok(Outcome::Climate(Some(ClimateState{hvac_on:Some(false),driver_temp_celsius:Some(21.0),passenger_temp_celsius:Some(22.0),min_temp_celsius:Some(15.0),max_temp_celsius:Some(28.0),sampled_ms:Some(now_ms()),..Default::default()}))),
                Operation::ClimateCommand(_)=>{
                    let n=commands.fetch_add(1,Ordering::SeqCst);
                    if n>0 {let _=cancel.changed().await;closed.fetch_add(1,Ordering::SeqCst);return Err("synthetic cancelled command".into());}
                    Ok(Outcome::Climate(None))
                },
                Operation::Pair=>unreachable!(),
            }})
        }
    }));
    manager.command(configure("5YJ30123456789ABC",true)).await.unwrap();manager.command(Command::ClimatePair).await.unwrap();climate_ready(&manager).await;
    manager.command(Command::ClimateQuery).await.unwrap();climate_ready(&manager).await;
    let context=feed.climate().context;grant.send_replace(Some(context));
    revoke_poll.store(true,Ordering::SeqCst);
    let denied=feed.request_climate(context,Some(ClimateCommand::Power{on:true}),grant.subscribe()).unwrap();
    assert!(!tokio::time::timeout(Duration::from_secs(1),denied).await.unwrap().unwrap());assert_eq!(commands.load(Ordering::SeqCst),0,"revocation during Poll cleanup forbids sending");
    let reply=feed.request_climate(context,Some(ClimateCommand::Power{on:true}),grant.subscribe()).unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(1),reply).await.unwrap().unwrap());assert_eq!(commands.load(Ordering::SeqCst),1);
    assert_eq!(manager.status().climate.last_command_status,"confirmed");assert!(feed.climate().current().is_none(),"ACK cannot fabricate current state");
    let query=feed.request_climate(context,None,grant.subscribe()).unwrap();assert!(tokio::time::timeout(Duration::from_secs(1),query).await.unwrap().unwrap());
    assert_eq!(feed.climate().current().unwrap().driver_temp_celsius,Some(21.0));
    let reply=feed.request_climate(context,Some(ClimateCommand::Power{on:false}),grant.subscribe()).unwrap();
    tokio::time::timeout(Duration::from_secs(1),async{while commands.load(Ordering::SeqCst)!=2{tokio::task::yield_now().await;}}).await.unwrap();
    drop(reply);
    tokio::time::timeout(Duration::from_secs(1),async{while closed.load(Ordering::SeqCst)!=1{tokio::task::yield_now().await;}}).await.unwrap();
    assert_eq!(commands.load(Ordering::SeqCst),2);assert_eq!(manager.status().climate.last_command_status,"unconfirmed");
    assert_eq!(manager.status().climate.phase,"cancelled");
    manager.command(Command::ClimateQuery).await.unwrap();climate_ready(&manager).await;
    assert!(manager.status().climate.fresh,"a cancelled native command can be recovered with an explicit read");
    manager.command(Command::Disconnect).await.unwrap();assert_ne!(feed.climate().context,context);assert!(feed.climate().current().is_none());
    stop.send(true).unwrap();actor.await.unwrap();
}

#[tokio::test]
async fn driver_authorization_is_separate_and_ack_never_changes_measured_temperatures() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path(), Feed::new());
    let commands = Arc::new(AtomicUsize::new(0));
    let observed = commands.clone();
    let (stop, rx) = watch::channel(false);
    let actor = tokio::spawn(
        manager
            .clone()
            .run_with(rx, move |operation, _, _, mut stop| {
                let commands = observed.clone();
                Box::pin(async move {
                    match operation {
                        Operation::Poll => {
                            let _ = stop.changed().await;
                            Ok(Outcome::Done)
                        }
                        Operation::ClimatePair => Ok(Outcome::Done),
                        Operation::ClimateQuery => Ok(Outcome::Climate(Some(ClimateState {
                            hvac_on: Some(true),
                            driver_temp_celsius: Some(21.0),
                            passenger_temp_celsius: Some(22.0),
                            min_temp_celsius: Some(15.0),
                            max_temp_celsius: Some(28.0),
                            sampled_ms: Some(now_ms()),
                            ..Default::default()
                        }))),
                        Operation::ClimateCommand(_) => {
                            commands.fetch_add(1, Ordering::SeqCst);
                            Ok(Outcome::Climate(None))
                        }
                        Operation::Pair => unreachable!(),
                    }
                })
            }),
    );
    manager
        .command(configure("5YJ30123456789ABC", true))
        .await
        .unwrap();
    let monitor = manager
        .state
        .lock()
        .unwrap()
        .saved
        .as_ref()
        .unwrap()
        .private_key
        .clone();
    let monitor_bytes = std::fs::read(&monitor).unwrap();
    assert!(!manager.status().climate.authorized);
    assert!(
        manager
            .command(Command::ClimateCommand(ClimateCommand::Power { on: true }))
            .await
            .is_err()
    );
    manager.command(Command::ClimatePair).await.unwrap();
    climate_ready(&manager).await;
    assert!(manager.status().climate.authorized);
    let driver = manager
        .state
        .lock()
        .unwrap()
        .saved
        .as_ref()
        .unwrap()
        .control_private_key
        .clone()
        .unwrap();
    assert_ne!(driver, monitor);
    assert_eq!(std::fs::read(&monitor).unwrap(), monitor_bytes);
    manager.command(Command::ClimateQuery).await.unwrap();
    climate_ready(&manager).await;
    let before = manager.status().climate.state.clone();
    assert!(manager.status().climate.fresh);
    manager
        .command(Command::ClimateCommand(ClimateCommand::SetTemperature {
            driver_celsius: 24.0,
            passenger_celsius: 24.5,
        }))
        .await
        .unwrap();
    climate_ready(&manager).await;
    assert_eq!(commands.load(Ordering::SeqCst), 1);
    assert_eq!(manager.status().climate.last_command_status, "confirmed");
    assert_eq!(
        manager.status().climate.state,
        before,
        "ACK cannot invent measured state"
    );
    assert!(
        manager
            .command(Command::ClimateCommand(ClimateCommand::SetTemperature {
                driver_celsius: 35.0,
                passenger_celsius: 24.5
            }))
            .await
            .is_err()
    );
    manager
        .state
        .lock()
        .unwrap()
        .climate
        .state
        .as_mut()
        .unwrap()
        .sampled_ms = Some(now_ms().saturating_sub(31_000));
    assert!(!manager.status().climate.fresh);
    assert!(
        manager
            .command(Command::ClimateCommand(ClimateCommand::SetTemperature {
                driver_celsius: 24.0,
                passenger_celsius: 24.5
            }))
            .await
            .is_err()
    );
    assert_eq!(commands.load(Ordering::SeqCst), 1);
    manager.command(Command::Disconnect).await.unwrap();
    assert!(manager.status().climate.state.is_none());
    assert!(manager.status().climate.received_ms.is_none());
    stop.send(true).unwrap();
    actor.await.unwrap();
    assert!(driver.exists());
    assert!(monitor.exists());
}

#[tokio::test]
async fn driver_pair_cancel_resumes_monitor_but_explicit_disconnect_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path(), Feed::new());
    let observed = Arc::new(Observed::default());
    let (stop, rx) = watch::channel(false);
    let actor = tokio::spawn(manager.clone().run_with(rx, runner(observed.clone())));
    manager
        .command(configure("5YJ30123456789ABC", true))
        .await
        .unwrap();
    started(&observed, 1).await;
    manager.command(Command::ClimatePair).await.unwrap();
    started(&observed, 2).await;
    assert!(!manager.status().climate.authorized);
    assert!(
        manager.command(Command::ClimateQuery).await.is_err(),
        "cannot replace an active grant request"
    );
    manager.command(Command::Cancel).await.unwrap();
    started(&observed, 3).await;
    assert_eq!(manager.status().climate.phase, "cancelled");
    assert_eq!(observed.closed.load(Ordering::SeqCst), 2);
    manager.command(Command::ClimateQuery).await.unwrap();
    started(&observed, 4).await;
    manager.command(Command::Disconnect).await.unwrap();
    assert_eq!(observed.closed.load(Ordering::SeqCst), 4);
    assert_eq!(observed.active.load(Ordering::SeqCst), 0);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(observed.started.load(Ordering::SeqCst), 4);
    stop.send(true).unwrap();
    actor.await.unwrap();
}

#[tokio::test]
async fn aborting_service_actor_cannot_detach_a_noncooperative_child_or_write_late_readings() {
    struct Dropped(Arc<AtomicUsize>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path(), Feed::new());
    let started = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let observed_start = started.clone();
    let observed_drop = dropped.clone();
    let (_stop, rx) = watch::channel(false);
    let actor = tokio::spawn(manager.clone().run_with(rx, move |_, _, feed, _| {
        let started = observed_start.clone();
        let dropped = observed_drop.clone();
        Box::pin(async move {
            let _drop = Dropped(dropped);
            started.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(150)).await;
            feed.status(true, true, "reading");
            std::future::pending().await
        })
    }));
    manager
        .command(configure("5YJ30123456789ABC", true))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    actor.abort();
    assert!(actor.await.unwrap_err().is_cancelled());
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "actor join synchronously drops its inline child"
    );
    tokio::time::sleep(Duration::from_millis(180)).await;
    assert!(!manager.feed.snapshot().connected);
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn external_reload_cannot_carry_driver_authority_or_freshness_to_a_different_vehicle() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path(), Feed::new());
    let (stop, rx) = watch::channel(false);
    let actor = tokio::spawn(manager.clone().run_with(rx, |_, _, _, mut stop| {
        Box::pin(async move {
            if !*stop.borrow() {
                let _ = stop.changed().await;
            }
            Ok(Outcome::Done)
        })
    }));
    manager
        .command(configure("5YJ30123456789ABC", true))
        .await
        .unwrap();
    {
        let mut s = manager.state.lock().unwrap();
        s.climate.authorized = true;
        s.climate.key_ready = true;
        s.climate.state = Some(ClimateState {
            sampled_ms: Some(now_ms()),
            min_temp_celsius: Some(15.0),
            max_temp_celsius: Some(28.0),
            ..Default::default()
        });
        s.climate.received_ms = Some(now_ms());
    }
    let mut external = manager.state.lock().unwrap().saved.clone().unwrap();
    external.vin = "5YJ30123456789ABD".into();
    external.control_private_key = None;
    f50_server::settings::write_private(
        &manager.directory.join("config.json"),
        &serde_json::to_vec(&external).unwrap(),
    )
    .unwrap();
    manager.command(Command::Connect).await.unwrap();
    let status = manager.status();
    assert!(!status.climate.authorized);
    assert!(!status.climate.key_ready);
    assert!(status.climate.state.is_none());
    assert!(manager.command(Command::ClimateQuery).await.is_err());
    assert!(
        manager
            .command(Command::ClimateCommand(ClimateCommand::Power { on: true }))
            .await
            .is_err()
    );
    f50_server::settings::write_private(
        &manager.directory.join("config.json"),
        b"{invalid configuration}",
    )
    .unwrap();
    assert!(manager.command(Command::Connect).await.is_err());
    assert!(!manager.status().configured);
    assert_eq!(manager.status().phase, "configuration_error");
    stop.send(true).unwrap();
    actor.await.unwrap();
}

#[tokio::test]
async fn command_timeout_resync_and_missing_ack_remain_unconfirmed_without_resend() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path(), Feed::new());
    let commands = Arc::new(AtomicUsize::new(0));
    let polls = Arc::new(AtomicUsize::new(0));
    let observed_commands = commands.clone();
    let observed_polls = polls.clone();
    let (stop, rx) = watch::channel(false);
    let actor = tokio::spawn(
        manager
            .clone()
            .run_with(rx, move |operation, _, _, mut stop| {
                let commands = observed_commands.clone();
                let polls = observed_polls.clone();
                Box::pin(async move {
                    match operation {
                        Operation::Poll => {
                            polls.fetch_add(1, Ordering::SeqCst);
                            if !*stop.borrow() {
                                let _ = stop.changed().await;
                            }
                            Ok(Outcome::Done)
                        }
                        Operation::ClimatePair => Ok(Outcome::Done),
                        Operation::ClimateQuery => Ok(Outcome::Climate(Some(ClimateState {
                            hvac_on: Some(false),
                            driver_temp_celsius: Some(21.0),
                            passenger_temp_celsius: Some(22.0),
                            min_temp_celsius: Some(15.0),
                            max_temp_celsius: Some(28.0),
                            sampled_ms: Some(now_ms()),
                            ..Default::default()
                        }))),
                        Operation::ClimateCommand(_) => {
                            match commands.fetch_add(1, Ordering::SeqCst) {
                                0 => Err(f50_core::tesla::Error::Resync.into()),
                                1 => Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into()),
                                2 => Err(f50_core::tesla::Error::Malformed.into()),
                                _ => {
                                    if !*stop.borrow() {
                                        let _ = stop.changed().await;
                                    }
                                    Err("synthetic command wait cancelled".into())
                                }
                            }
                        }
                        Operation::Pair => unreachable!(),
                    }
                })
            }),
    );
    manager
        .command(configure("5YJ30123456789ABC", true))
        .await
        .unwrap();
    manager.command(Command::ClimatePair).await.unwrap();
    climate_ready(&manager).await;
    for expected in 1..=3 {
        manager.command(Command::ClimateQuery).await.unwrap();
        climate_ready(&manager).await;
        let measured = manager.status().climate.state.clone();
        let polls_before = polls.load(Ordering::SeqCst);
        manager
            .command(Command::ClimateCommand(ClimateCommand::Power { on: true }))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while manager.status().climate.phase != "failed"
                || polls.load(Ordering::SeqCst) <= polls_before
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(manager.status().climate.last_command_status, "unconfirmed");
        assert_eq!(manager.status().climate.state, measured);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            commands.load(Ordering::SeqCst),
            expected,
            "only the explicit command is attempted"
        );
    }
    manager.command(Command::ClimateQuery).await.unwrap();
    climate_ready(&manager).await;
    manager
        .command(Command::ClimateCommand(ClimateCommand::Power { on: true }))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while commands.load(Ordering::SeqCst) != 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    manager.command(Command::Cancel).await.unwrap();
    assert_eq!(manager.status().climate.last_command_status, "unconfirmed");
    assert_eq!(manager.status().climate.phase, "cancelled");
    assert_eq!(commands.load(Ordering::SeqCst), 4);
    stop.send(true).unwrap();
    actor.await.unwrap();
}

#[tokio::test]
async fn managed_settings_keep_cli_monitor_identity_and_legacy_supervisor_honors_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let monitor = dir.path().join("monitor.pk8");
    let driver = dir.path().join("driver.pk8");
    worker::keygen(&monitor).unwrap();
    worker::keygen(&driver).unwrap();
    let path = dir.path().join("managed.json");
    let saved = Saved {
        vin: "5YJ30123456789ABC".into(),
        private_key: monitor.clone(),
        enabled: false,
        control_private_key: Some(driver.clone()),
    };
    f50_server::settings::write_private(&path, &serde_json::to_vec(&saved).unwrap()).unwrap();
    let config = load_monitor_config(&path).unwrap();
    assert_eq!(config.private_key, monitor);
    assert_ne!(config.private_key, driver);
    assert_eq!(config.vin, saved.vin);
    let feed = Feed::new();
    let mut changed = feed.subscribe();
    let (stop, rx) = watch::channel(false);
    let supervisor = tokio::spawn(worker::supervise_on(
        path.clone(),
        feed.clone(),
        rx,
        "synthetic-invalid-bus-address".into(),
        0,
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while feed.snapshot().phase != "disabled" {
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(!feed.snapshot().enabled);
    assert!(!feed.snapshot().connected);
    stop.send(true).unwrap();
    supervisor.await.unwrap();
    f50_server::settings::write_private(
        &path,
        &serde_json::to_vec(&serde_json::json!({"vin":saved.vin,"private_key":monitor})).unwrap(),
    )
    .unwrap();
    assert_eq!(load_monitor_config(&path).unwrap().private_key, monitor);
}
