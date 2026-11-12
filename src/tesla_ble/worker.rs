//! Service-owned authenticated, strictly read-only Tesla poller. Separate from phone lifecycle.
use super::{Result, transport::Link};
use f50_core::tesla::{Category, Client, Identity, Update, Error as ProtocolError};
use f50_server::vehicle::{Feed, now_ms};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub vin: String,
    pub private_key: PathBuf,
}
pub async fn supervise(
    settings_path: PathBuf,
    feed: Arc<Feed>,
    service_stop: watch::Receiver<bool>,
) {
    supervise_on(settings_path, feed, service_stop, crate::board::BUS.into(), 0).await
}
pub async fn supervise_on(settings_path: PathBuf, feed: Arc<Feed>, mut service_stop: watch::Receiver<bool>, bus: String, adapter: u16) {
    let mut retry = feed.retries();
    loop {
        if *service_stop.borrow() {
            break;
        }
        let config = settings_path
            .to_str()
            .ok_or("invalid Tesla configuration path")
            .map_err(|e| e.to_owned())
            .and_then(|path| match f50_server::settings::read(path, 4096) {
                Ok(bytes) => super::management::monitor_configuration(&bytes)
                    .map(Some)
                    .map_err(|_| "invalid Tesla BLE configuration".to_owned()),
                Err(_) if !settings_path.exists() => Ok(None),
                Err(_) => Err("Tesla BLE configuration unreadable".into()),
            });
        let config = match config {
            Ok(Some((config, true))) => {
                feed.reset();
                feed.status(true, false, "connecting");
                config
            }
            Ok(Some((_, false))) => {
                feed.reset();
                feed.status(false, false, "disabled");
                tokio::select! {_=service_stop.changed()=>break,_=retry.changed()=>continue}
            }
            other => {
                if other.is_err() {
                    feed.status(true, false, "configuration_error");
                } else {
                    feed.status(false, false, "unconfigured");
                }
                tokio::select! {_=service_stop.changed()=>break,_=retry.changed()=>continue}
            }
        };
        let (cancel, rx) = watch::channel(false);
        let reading = run_on(config.clone(), feed.clone(), rx, &bus, adapter);
        tokio::pin!(reading);
        let mut completed = false;
        let stopping = tokio::select! {
            _=service_stop.changed()=>true,
            _=retry.changed()=>false,
            result=&mut reading=>{
                completed=true;
                if result.is_err(){if feed.snapshot().phase=="connecting"{feed.status(true,false,"configuration_error");}f50_server::diag_log!(Warn,"tesla","Tesla BLE读取停止；保留密钥与配对，修正现场条件后可执行tesla_retry");}
                tokio::select!{_=service_stop.changed()=>true,_=retry.changed()=>false}
            }
        };
        let _ = cancel.send(true);
        if !completed {
            let _ = reading.await;
        }
        if stopping {
            feed.status(true, false, "stopped");
            break;
        }
    }
    feed.status(feed.snapshot().enabled, false, "stopped");
}
pub fn keygen(path: &Path) -> Result<Vec<u8>> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let identity = Identity::generate();
    // create_new prevents replacement of a vehicle-authorized identity, including symlinks.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&identity.pkcs8()?)?;
    file.sync_all()?;
    Ok(identity.public_key())
}
pub async fn enroll(config: Config, stop: watch::Receiver<bool>) -> Result<()> {
    enroll_on(config, stop, crate::board::BUS, 0).await
}
pub async fn enroll_on(config: Config, stop: watch::Receiver<bool>, bus: &str, adapter: u16) -> Result<()> {
    enroll_with_progress(config, stop, bus, adapter, |phase| match phase {
        "awaiting_confirmation" => println!("已发送只读Vehicle Monitor公钥授权请求。请在车内刷NFC钥匙卡并确认，等待车辆验证；私钥不离开本机。"),
        "authorized" => println!("Tesla已验证本机公钥授权；未发送车辆控制命令。"),
        _ => {},
    }).await
}
pub(super) async fn enroll_with_progress(config: Config, stop: watch::Receiver<bool>, bus: &str, adapter: u16, progress: impl Fn(&'static str) + Send) -> Result<()> {
    enroll_for(config, stop, bus, adapter, f50_core::tesla::EnrollmentPermission::VehicleMonitor, progress).await
}
pub(super) async fn enroll_for(config: Config, mut stop: watch::Receiver<bool>, bus: &str, adapter: u16, permission: f50_core::tesla::EnrollmentPermission, progress: impl Fn(&'static str) + Send) -> Result<()> {
    let bytes = f50_server::settings::read(
        config
            .private_key
            .to_str()
            .ok_or("invalid Tesla key path")?,
        4096,
    )?;
    let identity = Identity::from_pkcs8(&bytes)?;
    let request = identity.enrollment_request_for(permission);
    let mut client = Client::new(&config.vin, identity)?;
    let mut link = Link::open_on(bus, &client.local_name(), stop.clone(), adapter).await?;
    let result=async{
        link.send(&request,&mut stop).await?;
        progress("awaiting_confirmation");
        for _ in 0..12 {
            if *stop.borrow(){return Err::<(),Box<dyn std::error::Error+Send+Sync>>("Tesla enrollment cancelled".into());}
            let handshake=tokio::time::timeout(Duration::from_secs(4),async{
                link.send(&client.hello(),&mut stop).await?;
                loop {let bytes=link.receive(&mut stop).await?;if client.accept_hello(&bytes)?{return Ok::<_,Box<dyn std::error::Error+Send+Sync>>(());}}
            }).await;
            if matches!(handshake,Ok(Ok(()))){progress("authorized");return Ok(());}
            tokio::select!{_=stop.changed()=>return Err("Tesla enrollment cancelled".into()),_=tokio::time::sleep(Duration::from_secs(2))=>{}}
        }Err("Tesla公钥授权未在有界窗口内完成；保留密钥，确认车辆权限后重试".into())
    }.await;
    let cleanup = link.close().await;
    cleanup?;
    result
}
pub async fn run(config: Config, feed: Arc<Feed>, stop: watch::Receiver<bool>) -> Result<()> {
    run_on(config, feed, stop, crate::board::BUS, 0).await
}
pub(super) async fn run_on(config: Config, feed: Arc<Feed>, mut stop: watch::Receiver<bool>, bus: &str, adapter: u16) -> Result<()> {
    let bytes = f50_server::settings::read(
        config
            .private_key
            .to_str()
            .ok_or("invalid Tesla key path")?,
        4096,
    )?;
    let identity = Identity::from_pkcs8(&bytes)?;
    let mut client = Client::new(&config.vin, identity)?;
    let mut failures = 0u8;
    let mut health = ReadHealth::default();
    let result=async{
        loop {
            if *stop.borrow(){break Ok(());}
            feed.status(true,false,"connecting");
            let mut link=match Link::open_on(bus,&client.local_name(),stop.clone(),adapter).await {
                Ok(link)=>link,
                Err(error)=>{
                    if *stop.borrow(){break Ok(());}
                    let failure=error.downcast_ref::<super::transport::Failure>();
                    let phase=failure.map_or("private_bus_unavailable",|e|e.stage);feed.status(true,false,phase);
                    if failure.is_some_and(|e|e.cleanup_uncertain){break Err("Tesla BLE setup cleanup failed; no automatic retry".into());}
                    failures+=1;if failures>=3{break Err("Tesla BLE connection stopped after three attempts".into());}
                    tokio::select!{_=stop.changed()=>break Ok(()),_=tokio::time::sleep(Duration::from_secs(u64::from(failures)*2))=>{}}continue;
                }
            };
            feed.status(true,true,"authorizing");
            let mut phase="authorization_failed";
            let reading=read(&mut link,&mut client,&feed,&mut stop,&mut failures,&mut phase,&mut health).await;
            let cleanup=link.close().await;feed.status(true,false,if *stop.borrow(){"stopped"}else{phase});
            if cleanup.is_err(){feed.status(true,false,"cleanup_failed");break Err("Tesla BLE cleanup failed; not retrying an uncertain connection".into());}
            if *stop.borrow(){break Ok(());}
            if reading.as_ref().err().is_some_and(|e|e.downcast_ref::<f50_core::tesla::Error>()==Some(&f50_core::tesla::Error::NotAuthorized)) {feed.status(true,false,"not_authorized");break Err("Tesla has not authorized this receiver public key".into());}
            if reading.as_ref().err().is_some_and(|e|e.downcast_ref::<ProtocolError>()==Some(&ProtocolError::Exhausted)) {
                feed.status(true,false,"read_attempts_exhausted");break Err("Tesla read attempts exhausted; explicit retry required".into());
            }
            failures+=1;
            if failures>=3{break Err("Tesla BLE read failed after three attempts".into());}
            tokio::select!{_=stop.changed()=>break Ok(()),_=tokio::time::sleep(Duration::from_secs(u64::from(failures)*2))=>{}}
        }
    }.await;
    if *stop.borrow() {
        feed.status(true, false, "stopped");
    }
    result
}
async fn read(
    link: &mut Link,
    client: &mut Client,
    feed: &Feed,
    stop: &mut watch::Receiver<bool>,
    failures: &mut u8,
    phase: &mut &'static str,
    health: &mut ReadHealth,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(4), async {
        link.send(&client.hello(), stop).await?;
        loop {
            let packet = link.receive(stop).await?;
            if client.accept_hello(&packet)? {
                return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
            }
        }
    })
    .await??;
    feed.status(true, true, "reading");
    let mut source = AuthenticatedReads { link, client };
    loop {
        poll_round(&mut source, feed, stop, phase, health).await?;
        *failures = 0;
        tokio::select! {_=stop.changed()=>return Ok(()),_=tokio::time::sleep(Duration::from_secs(5))=>{}}
    }
}

trait ReadSource {
    fn query(&mut self, category: Category, stop: &mut watch::Receiver<bool>)
        -> impl std::future::Future<Output = Result<Update>> + Send;
}
struct AuthenticatedReads<'a> { link: &'a mut Link, client: &'a mut Client }
impl ReadSource for AuthenticatedReads<'_> {
    async fn query(&mut self, category: Category, stop: &mut watch::Receiver<bool>) -> Result<Update> {
        for attempt in 0..2 {
            let outcome = tokio::time::timeout(Duration::from_secs(5), async {
                self.link.send(&self.client.query(category)?, stop).await?;
                loop {
                    let packet = self.link.receive(stop).await?;
                    if let Some(update) = self.client.accept_response(&packet, now_ms())? {
                        return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(update);
                    }
                }
            }).await;
            let outcome = match outcome { Ok(result) => result, Err(error) => Err(error.into()) };
            if outcome.as_ref().err().is_some_and(|e| e.downcast_ref::<ProtocolError>() == Some(&ProtocolError::Resync)) && attempt == 0 {
                continue;
            }
            // A timeout or bare fault can leave a request pending. Retire its UUID
            // before another category is sent, preserving this authenticated session.
            if outcome.is_err() { self.client.cancel_query(); }
            return outcome;
        }
        unreachable!()
    }
}
#[derive(Default)]
struct ReadHealth { failures: [u8; 2] }
async fn poll_round(source: &mut impl ReadSource, feed: &Feed, stop: &mut watch::Receiver<bool>, phase: &mut &'static str, health: &mut ReadHealth) -> Result<()> {
    for (index, category) in [Category::Charge, Category::Tires].into_iter().enumerate() {
        if *stop.borrow() { return Ok(()); }
        if health.failures[index] >= 3 { continue; }
        *phase = match category { Category::Charge => "charge_read_failed", Category::Tires => "tire_read_failed" };
        match source.query(category, stop).await {
            Ok(update) => { health.failures[index] = 0; feed.apply(update); }
            Err(error) if matches!(error.downcast_ref::<ProtocolError>(), Some(ProtocolError::VehicleFault(1 | 2 | 11))) => {
                health.failures[index] += 1;
                let paused = health.failures[index] >= 3;
                feed.read_failed(category, paused);
                if paused {
                    f50_server::diag_log!(Warn,"tesla","Tesla {:?}连续三次暂时失败，已暂停该类；其他类别继续，tesla_retry可重新读取",category);
                }
            }
            Err(error) => return Err(error),
        }
    }
    if health.failures.iter().all(|n| *n >= 3) { return Err(ProtocolError::Exhausted.into()); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_generation_is_private_and_never_overwrites_authorized_identity() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("synthetic-identity.pk8");
        let public = keygen(&path).unwrap();
        assert_eq!(public.len(), 65);
        assert_eq!(public[0], 4);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let before = zeroize::Zeroizing::new(std::fs::read(&path).unwrap());
        assert!(keygen(&path).is_err());
        assert_eq!(&*before, &std::fs::read(&path).unwrap());
    }
    #[tokio::test]
    async fn configuration_failure_waits_for_explicit_retry_and_stops_without_bluez() {
        let feed = Feed::new();
        feed.status(true, false, "connecting");
        let (stop, rx) = watch::channel(false);
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("tesla-ble.json");
        f50_server::settings::write_private(
            &settings,
            br#"{"vin":"5YJ30123456789ABC","private_key":"/synthetic/nonexistent-tesla-key"}"#,
        )
        .unwrap();
        let task = tokio::spawn(supervise(settings.clone(), feed.clone(), rx));
        let mut changes = feed.subscribe();
        tokio::time::timeout(Duration::from_secs(1), async {
            while feed.snapshot().phase != "configuration_error" {
                changes.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(!task.is_finished());
        std::fs::remove_file(&settings).unwrap();
        assert!(feed.request_retry());
        tokio::time::timeout(Duration::from_secs(1), async {
            while feed.snapshot().phase != "unconfigured" {
                changes.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(!feed.snapshot().enabled);
        f50_server::settings::write_private(
            &settings,
            br#"{"vin":"5YJ30123456789ABC","private_key":"/synthetic/nonexistent-tesla-key"}"#,
        )
        .unwrap();
        assert!(feed.request_retry());
        tokio::time::timeout(Duration::from_secs(1), async {
            while feed.snapshot().phase != "configuration_error" {
                changes.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        task.await.unwrap();
        assert_eq!(feed.snapshot().phase, "stopped");
    }
}

#[cfg(test)] mod recovery_tests {
    use super::*;
    use f50_core::tesla::Reading;
    use std::collections::VecDeque;
    #[derive(Default)] struct Script { calls: Vec<Category>, charge_busy: bool }
    impl ReadSource for Script {
        async fn query(&mut self, category: Category, _: &mut watch::Receiver<bool>) -> Result<Update> {
            self.calls.push(category);
            if category == Category::Charge && self.charge_busy { return Err(ProtocolError::VehicleFault(1).into()); }
            Ok(match category { Category::Charge => Update::Charge { soc_pct: None, range_km: None }, Category::Tires => Update::Tires([None;4]) })
        }
    }
    #[tokio::test] async fn charge_busy_does_not_block_tire_reads() {
        let mut source = Script { charge_busy: true, ..Default::default() };
        let feed=Feed::new(); feed.status(true,true,"reading");
        let (_cancel,mut stop)=watch::channel(false); let mut phase="reading";
        poll_round(&mut source,&feed,&mut stop,&mut phase,&mut ReadHealth::default()).await.unwrap();
        assert_eq!(source.calls, vec![Category::Charge,Category::Tires]);
    }

    #[derive(Default)] struct Replies { replies: [VecDeque<Result<Update>>;2], calls: Vec<Category> }
    fn value(n: f64) -> Reading { let now=now_ms(); Reading { value:n,received_ms:now,sample_received_ms:now,sampled_ms:Some(now),sampled_time_trusted:true } }
    fn update(category: Category) -> Update { match category { Category::Charge=>Update::Charge { soc_pct:Some(value(0.0)),range_km:Some(value(300.0)) },Category::Tires=>Update::Tires([Some(value(250.0));4]) } }
    impl ReadSource for Replies {
        async fn query(&mut self, category: Category, _: &mut watch::Receiver<bool>) -> Result<Update> {
            self.calls.push(category);
            self.replies[usize::from(category==Category::Tires)].pop_front().unwrap_or_else(||Ok(update(category)))
        }
    }
    #[tokio::test] async fn each_category_has_its_own_bounded_budget_and_pause_remains_visible() {
        for failed in [Category::Charge,Category::Tires] {
            let index=usize::from(failed==Category::Tires); let mut source=Replies::default();
            source.replies[index]=(0..3).map(|_|Err(ProtocolError::VehicleFault(1).into())).collect();
            let feed=Feed::new(); feed.status(true,true,"reading");
            feed.apply(update(Category::Charge));feed.apply(update(Category::Tires));
            let (_cancel,mut stop)=watch::channel(false);let mut phase="reading";let mut health=ReadHealth::default();
            for _ in 0..4 { poll_round(&mut source,&feed,&mut stop,&mut phase,&mut health).await.unwrap(); }
            assert_eq!(source.calls.iter().filter(|c|**c==failed).count(),3);
            assert_eq!(source.calls.iter().filter(|c|**c!=failed).count(),4);
            let snapshot=feed.snapshot();
            assert_eq!(snapshot.phase,if failed==Category::Charge {"charge_paused"}else{"tires_paused"});
            assert_eq!(snapshot.values.soc_pct,if failed==Category::Charge {None}else{Some(0.0)});
            assert_eq!(snapshot.values.tires_kpa,if failed==Category::Tires {[None;4]}else{[Some(250.0);4]});
        }
    }
    #[tokio::test] async fn category_success_resets_only_its_own_consecutive_failures() {
        let mut source=Replies::default();
        source.replies[0]=VecDeque::from([Err(ProtocolError::VehicleFault(2).into()),Ok(update(Category::Charge)),Err(ProtocolError::VehicleFault(11).into())]);
        let feed=Feed::new();feed.status(true,true,"reading");let (_cancel,mut stop)=watch::channel(false);
        let mut health=ReadHealth::default();let mut phase="reading";
        for expected in [1,0,1] { poll_round(&mut source,&feed,&mut stop,&mut phase,&mut health).await.unwrap();assert_eq!(health.failures,[expected,0]); }
        poll_round(&mut source,&feed,&mut stop,&mut phase,&mut health).await.unwrap();
        assert_eq!(feed.snapshot().values.soc_pct,Some(0.0));
    }
    #[tokio::test] async fn all_categories_exhaust_without_reconnecting_and_explicit_reload_recovers() {
        let mut source=Replies::default();for replies in &mut source.replies { *replies=(0..3).map(|_|Err(ProtocolError::VehicleFault(1).into())).collect(); }
        let feed=Feed::new();feed.status(true,true,"reading");let (_cancel,mut stop)=watch::channel(false);
        let mut health=ReadHealth::default();let mut phase="reading";
        for _ in 0..2 { poll_round(&mut source,&feed,&mut stop,&mut phase,&mut health).await.unwrap(); }
        let error=poll_round(&mut source,&feed,&mut stop,&mut phase,&mut health).await.unwrap_err();
        assert_eq!(error.downcast_ref::<ProtocolError>(),Some(&ProtocolError::Exhausted));
        let count=source.calls.len();assert!(poll_round(&mut source,&feed,&mut stop,&mut phase,&mut health).await.is_err());assert_eq!(source.calls.len(),count);
        // supervise resets the feed and run creates new budgets only on explicit retry.
        feed.reset();health=ReadHealth::default();
        poll_round(&mut source,&feed,&mut stop,&mut phase,&mut health).await.unwrap();
        assert_eq!(feed.snapshot().phase,"reading");assert_eq!(feed.snapshot().values.soc_pct,Some(0.0));assert_eq!(feed.snapshot().values.tires_kpa,[Some(250.0);4]);
    }
    #[tokio::test] async fn authentication_transport_and_local_timeout_errors_are_never_category_retries() {
        for error in [ProtocolError::Authentication,ProtocolError::NotAuthorized,ProtocolError::Malformed,ProtocolError::Rejected,ProtocolError::VehicleFault(5)] {
            let mut source=Replies::default();source.replies[0].push_back(Err(error.into()));
            let feed=Feed::new();feed.status(true,true,"reading");let (_cancel,mut stop)=watch::channel(false);
            assert!(poll_round(&mut source,&feed,&mut stop,&mut "reading",&mut ReadHealth::default()).await.is_err());assert_eq!(source.calls,[Category::Charge]);
        }
        let mut source=Replies::default();source.replies[0].push_back(Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into()));
        let feed=Feed::new();let (_cancel,mut stop)=watch::channel(false);
        assert!(poll_round(&mut source,&feed,&mut stop,&mut "reading",&mut ReadHealth::default()).await.is_err());assert_eq!(source.calls,[Category::Charge]);
    }
    #[tokio::test] async fn cancellation_prevents_the_next_category() {
        let mut source=Replies::default();let feed=Feed::new();let (cancel,mut stop)=watch::channel(true);
        poll_round(&mut source,&feed,&mut stop,&mut "reading",&mut ReadHealth::default()).await.unwrap();assert!(source.calls.is_empty());drop(cancel);
    }
}
