use super::*;
use crate::bluetooth::recovery_tests::{Remote, phones_fixture};
use f50_server::{
    Hub,
    receiver::{Credentials, InfoBase, Receiver},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const A: &str = "/org/bluez/hci0/dev_00_11_22_33_44_55";
const B: &str = "/org/bluez/hci0/dev_00_11_22_33_44_66";
struct NoIdentity;
impl f50_core::mfi::Identity for NoIdentity {
    fn certificate(&self) -> &[u8] {
        &[]
    }
    fn sign_digest(&self, _: &[u8; 32]) -> f50_core::Result<[u8; 64]> {
        Err(f50_core::Error::Authentication)
    }
}
struct Fixture {
    admin: Arc<Admin>,
    a: Arc<Remote>,
    b: Arc<Remote>,
    _bus: zbus::Connection,
    _dir: tempfile::TempDir,
    web: tokio::task::JoinHandle<()>,
    receiver: tokio::task::JoinHandle<std::io::Result<()>>,
    address: std::net::SocketAddr,
    control: std::net::SocketAddr,
    stop: watch::Sender<bool>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.web.abort();
        self.receiver.abort();
    }
}
async fn wait(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("synthetic state transition");
}
impl Fixture {
    async fn new() -> Self {
        let (manager, bus, a, b, old, dir) = phones_fixture().await;
        let (hub, commands) = Hub::with_phones(
            "123456".into(),
            "https://test.invalid".into(),
            old.phones.clone(),
        );
        assert!(hub.phones.handoff(0, A));
        a.fail_connect.store(true, Ordering::Release);
        b.fail_connect.store(true, Ordering::Release);
        let web = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = web.local_addr().unwrap();
        let router = f50_server::router(hub.clone());
        let web = tokio::spawn(async move {
            axum::serve(web, router).await.unwrap();
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let control = listener.local_addr().unwrap();
        let credentials = Arc::new(Credentials {
            pairing_store: None,
            endpoint: None,
            pairing_id: b"synthetic-accessory".to_vec(),
            signing_seed: zeroize::Zeroizing::new([0; 32]),
            mfi: Arc::new(NoIdentity),
            info: Vec::new(),
            info_base: InfoBase {
                name: "synthetic".into(),
                model: "synthetic".into(),
                manufacturer: "synthetic".into(),
                device_id: "02:00:00:00:00:01".into(),
                bluetooth_mac: "02:00:00:00:00:01".into(),
            },
            width: 800,
            height: 480,
        });
        let (stop, stopped) = watch::channel(false);
        let receiver = tokio::spawn(
            Receiver {
                credentials,
                hub: hub.clone(),
                pairing_window: Duration::from_secs(120),
            }
            .run(listener, commands, stopped),
        );
        let admin = Admin::new(
            manager,
            hub,
            "https://test.invalid".into(),
            dir.path().join("cowplay.uci"),
            dir.path().join("config"),
            dir.path().to_owned(),
            "cert".into(),
            "key".into(),
            address,
            control,
            None,
            (800, 480),
        )
        .unwrap();
        Self {
            admin,
            a,
            b,
            _bus: bus,
            _dir: dir,
            web,
            receiver,
            address,
            control,
            stop,
        }
    }
    async fn screen(&self) -> reqwest::Upgraded {
        let hub = &self.admin.hub;
        let client = hub
            .clients
            .admit_token(&hub.clients.token(), "screen", "synthetic-screen", "", "")
            .unwrap();
        let response = reqwest::Client::new()
            .get(format!("http://{}/ws", self.address))
            .header("Origin", "https://test.invalid")
            .header(
                "Cookie",
                format!("{}={}", f50_server::clients::COOKIE, client.id),
            )
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 101);
        let mut socket = response.upgrade().await.unwrap();
        let hello = serde_json::json!({"takeover": false}).to_string();
        let mask = [1u8, 2, 3, 4];
        let mut frame = vec![0x81];
        if hello.len() < 126 {
            frame.push(0x80 | hello.len() as u8);
        } else {
            frame.push(0xfe);
            frame.extend_from_slice(&(hello.len() as u16).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        frame.extend(hello.bytes().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        socket.write_all(&frame).await.unwrap();
        wait(|| hub.viewer_active()).await;
        socket
    }
    async fn phone(&self) -> TcpStream {
        let socket = TcpStream::connect(self.control).await.unwrap();
        wait(|| self.admin.hub.has_phone_session()).await;
        socket
    }
    async fn cmd(&self, cmd: &str, id: Option<&str>) {
        crate::console::run_cmd(
            &self.admin,
            crate::console::Cmd {
                cmd: cmd.into(),
                id: id.map(str::to_owned),
                generation: None,
            },
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn sixty_seconds_releases_phone_once_and_web_stays_available() {
    let f = Fixture::new().await;
    let mut phone = f.phone().await;
    let screen = f.screen().await;
    let now = Instant::now();
    f.admin.standby_step(now).await;
    drop(screen);
    wait(|| !f.admin.hub.viewer_active()).await;
    let now = f.admin.hub.viewer_offline_since();
    f.admin.standby_step(now).await;
    for i in 1..=5 {
        f.cmd("connect", None).await;
        f.admin
            .standby_step(now + Duration::from_secs(i * 10))
            .await;
    }
    f.admin.standby_step(now + Duration::from_secs(59)).await;
    assert!(f.admin.hub.has_phone_session());
    assert_eq!(f.a.disconnects.load(Ordering::Acquire), 0);
    f.admin.standby_step(now + IDLE).await;
    assert!(!f.admin.hub.has_phone_session());
    assert!(!f.admin.user_stopped());
    let count = f.a.disconnects.load(Ordering::Acquire);
    assert!(count > 0);
    let mut byte = [0];
    assert!(matches!(phone.read(&mut byte).await, Ok(0) | Err(_)));
    for i in 2..=4 {
        f.admin.standby_step(now + IDLE * i).await;
    }
    assert_eq!(
        f.a.disconnects.load(Ordering::Acquire),
        count,
        "one completed idle transition"
    );
    assert_eq!(
        f.a.calls.load(Ordering::Acquire),
        0,
        "HTTP requests cannot call an absent screen's phone"
    );
    assert!(
        reqwest::get(format!("http://{}/health", f.address))
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert!(
        f.admin
            .manager
            .saved_phones()
            .await
            .unwrap()
            .iter()
            .any(|p| p.id == A)
    );
    let _screen = f.screen().await;
    f.admin
        .standby_step(now + IDLE * 4 + Duration::from_secs(1))
        .await;
    assert_eq!(
        f.a.calls.load(Ordering::Acquire),
        1,
        "returning screen resumes the saved phone invitation"
    );
    assert!(
        !f.admin.hub.phones.snapshot().admission,
        "only a fresh iAP2 handoff may admit the phone"
    );
}

#[tokio::test]
async fn short_screen_loss_preserves_phone_and_selection() {
    let f = Fixture::new().await;
    let _phone = f.phone().await;
    let screen = f.screen().await;
    let now = Instant::now();
    f.admin.standby_step(now).await;
    let epoch = f.admin.hub.phones.epoch();
    drop(screen);
    wait(|| !f.admin.hub.viewer_active()).await;
    let now = f.admin.hub.viewer_offline_since();
    f.admin.standby_step(now).await;
    f.admin.standby_step(now + Duration::from_secs(59)).await;
    let _screen = f.screen().await;
    f.admin.standby_step(now + Duration::from_secs(61)).await;
    assert!(f.admin.hub.has_phone_session());
    assert_eq!(f.admin.hub.phones.epoch(), epoch);
    assert_eq!(f.a.disconnects.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn missing_phone_retries_with_backoff_despite_repeated_page_requests() {
    let f = Fixture::new().await;
    let phone = f.phone().await;
    let _screen = f.screen().await;
    let now = Instant::now();
    f.admin.standby_step(now).await;
    drop(phone);
    wait(|| !f.admin.hub.has_phone_session()).await;
    f.admin.standby_step(now).await;
    assert_eq!(f.a.calls.load(Ordering::Acquire), 1);
    // Backoff starts after the failed attempt and remote cleanup have completed.
    let now = Instant::now();
    for _ in 0..4 {
        f.cmd("connect", None).await;
    }
    f.admin.standby_step(now + Duration::from_secs(14)).await;
    assert_eq!(f.a.calls.load(Ordering::Acquire), 1);
    f.admin.standby_step(now + Duration::from_secs(16)).await;
    assert_eq!(f.a.calls.load(Ordering::Acquire), 2);
    f.admin.standby_step(now + Duration::from_secs(45)).await;
    assert_eq!(f.a.calls.load(Ordering::Acquire), 2);
    f.admin.standby_step(now + Duration::from_secs(47)).await;
    assert_eq!(f.a.calls.load(Ordering::Acquire), 3);
    f.admin.standby_step(now + Duration::from_secs(106)).await;
    assert_eq!(f.a.calls.load(Ordering::Acquire), 3);
    f.a.fail_connect.store(false, Ordering::Release);
    f.admin.standby_step(now + Duration::from_secs(108)).await;
    assert_eq!(
        f.a.calls.load(Ordering::Acquire),
        4,
        "returning phone is invited without a page reload or cleared pairing"
    );
    assert!(f.admin.manager.status().paired);
}

#[tokio::test]
async fn manual_disconnect_cancels_pending_invite_and_blocks_all_automatic_wakes() {
    let f = Fixture::new().await;
    let screen = f.screen().await;
    f.a.fail_connect.store(false, Ordering::Release);
    f.a.hold_connect_reply.store(true, Ordering::Release);
    f.admin
        .automatic
        .request(&f.admin, A.into(), f.admin.hub.phones.epoch());
    let attempt = tokio::spawn({
        let a = f.admin.clone();
        async move { a.standby_step(Instant::now()).await }
    });
    f.a.connecting.notified().await;
    assert!(!attempt.is_finished());
    f.cmd("disconnect", None).await;
    attempt.await.unwrap();
    let epoch = f.admin.hub.phones.epoch();
    let calls = f.a.calls.load(Ordering::Acquire);
    drop(screen);
    wait(|| !f.admin.hub.viewer_active()).await;
    let _screen = f.screen().await;
    f.cmd("connect", None).await;
    f.cmd("reconnect", None).await;
    f.admin.standby_step(Instant::now() + IDLE * 5).await;
    assert!(f.admin.user_stopped());
    assert_eq!(f.admin.hub.phones.epoch(), epoch);
    assert_eq!(f.a.calls.load(Ordering::Acquire), calls);
    assert!(
        f.a.disconnects.load(Ordering::Acquire) > 0,
        "pending remote operation was cancelled"
    );
    assert!(f.admin.manager.status().paired);
}

#[tokio::test]
async fn manual_switch_wins_and_future_retries_keep_b_even_when_a_is_preferred() {
    let f = Fixture::new().await;
    let _screen = f.screen().await;
    f.admin.update_policy(|p| p.preferred = A.into()).unwrap();
    f.a.fail_connect.store(false, Ordering::Release);
    f.a.hold_connect_reply.store(true, Ordering::Release);
    f.admin
        .automatic
        .request(&f.admin, A.into(), f.admin.hub.phones.epoch());
    let attempt = tokio::spawn({
        let a = f.admin.clone();
        async move { a.standby_step(Instant::now()).await }
    });
    f.a.connecting.notified().await;
    assert!(!attempt.is_finished());
    f.cmd("switchPhone", Some(B)).await;
    attempt.await.unwrap();
    wait(|| {
        f.b.calls.load(Ordering::Acquire) == 1
            && f.admin.automatic.manual.load(Ordering::Acquire) == 0
    })
    .await;
    let calls = f.a.calls.load(Ordering::Acquire);
    let now = Instant::now() + IDLE;
    f.cmd("connect", None).await;
    f.admin.standby_step(now).await;
    f.admin.standby_step(now + Duration::from_secs(16)).await;
    assert_eq!(f.admin.manager.selected_phone().as_deref(), Some(B));
    assert_eq!(
        f.a.calls.load(Ordering::Acquire),
        calls,
        "preferred A must not replace explicitly selected B"
    );
    assert!(f.b.calls.load(Ordering::Acquire) > 1);
}

#[tokio::test]
async fn fast_manual_switch_failure_gets_full_backoff_before_first_worker_event() {
    let f = Fixture::new().await;
    let _screen = f.screen().await;
    // No coordinator step has consumed this screen's first presence event.
    f.cmd("switchPhone", Some(B)).await;
    wait(|| {
        f.b.calls.load(Ordering::Acquire) == 1
            && f.admin.automatic.manual.load(Ordering::Acquire) == 0
    })
    .await;
    let now = Instant::now();
    let epoch = f.admin.hub.phones.epoch();
    f.cmd("connect", None).await;
    f.admin.standby_step(now + Duration::from_secs(14)).await;
    assert_eq!(f.b.calls.load(Ordering::Acquire), 1);
    assert_eq!(f.admin.hub.phones.epoch(), epoch);
    f.admin.standby_step(now + Duration::from_secs(16)).await;
    assert_eq!(f.b.calls.load(Ordering::Acquire), 2);
    assert_eq!(f.a.calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn pairing_cancel_and_manual_barrier_prevent_idle_or_retry_from_taking_over() {
    let f = Fixture::new().await;
    let now = Instant::now();
    f.admin.standby_step(now).await;
    let manual = f.admin.automatic.manual();
    let epoch = f.admin.hub.phones.epoch();
    f.admin.standby_step(now + IDLE).await;
    assert_eq!(
        f.admin.hub.phones.epoch(),
        epoch,
        "user action validating a phone wins over an old idle timer"
    );
    drop(manual);
    f.cmd("openPairWindow", None).await;
    let generation = f.admin.manager.status().generation;
    crate::console::run_cmd(
        &f.admin,
        crate::console::Cmd {
            cmd: "closePairWindow".into(),
            id: None,
            generation: Some(generation),
        },
    )
    .await
    .unwrap();
    assert!(f.admin.user_stopped());
    let epoch = f.admin.hub.phones.epoch();
    let _screen = f.screen().await;
    f.cmd("connect", None).await;
    f.admin.standby_step(now + IDLE * 10).await;
    assert_eq!(f.admin.hub.phones.epoch(), epoch);
    assert_eq!(f.a.calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn failed_idle_cleanup_backs_off_and_recovers_without_losing_pairing() {
    let f = Fixture::new().await;
    let channel = crate::bluetooth::recovery_tests::retain_channel(&f.admin.manager);
    let now = Instant::now();
    f.admin.standby_step(now).await;
    f.a.fail_disconnect.store(true, Ordering::Release);
    f.admin.standby_step(now + IDLE).await;
    let calls = f.a.disconnects.load(Ordering::Acquire);
    let epoch = f.admin.hub.phones.epoch();
    assert!(calls > 0);
    assert!(
        f.admin.manager.status().rfcomm,
        "failed cleanup retains the old channel"
    );
    f.admin.standby_step(now + Duration::from_secs(119)).await;
    assert_eq!(f.a.disconnects.load(Ordering::Acquire), calls);
    assert_eq!(f.admin.hub.phones.epoch(), epoch);
    f.admin.manager.complete(channel);
    f.a.fail_disconnect.store(false, Ordering::Release);
    f.admin.standby_step(now + Duration::from_secs(121)).await;
    let done = f.a.disconnects.load(Ordering::Acquire);
    assert!(done > calls);
    f.admin.standby_step(now + IDLE * 10).await;
    assert_eq!(f.a.disconnects.load(Ordering::Acquire), done);
    assert!(f.admin.manager.status().paired);
}

#[tokio::test]
async fn losing_screen_cancels_remote_invite_and_does_not_retry_until_screen_returns() {
    let f = Fixture::new().await;
    let screen = f.screen().await;
    f.a.fail_connect.store(false, Ordering::Release);
    f.a.hold_connect_reply.store(true, Ordering::Release);
    f.admin
        .automatic
        .request(&f.admin, A.into(), f.admin.hub.phones.epoch());
    let attempt = tokio::spawn({
        let a = f.admin.clone();
        async move { a.standby_step(Instant::now()).await }
    });
    f.a.connecting.notified().await;
    assert!(!attempt.is_finished());
    drop(screen);
    tokio::time::timeout(Duration::from_secs(1), attempt)
        .await
        .unwrap()
        .unwrap();
    let calls = f.a.calls.load(Ordering::Acquire);
    let now = Instant::now();
    f.admin.standby_step(now).await;
    f.admin.standby_step(now + IDLE * 3).await;
    assert_eq!(f.a.calls.load(Ordering::Acquire), calls);
    assert!(f.a.disconnects.load(Ordering::Acquire) > 0);
    assert!(f.admin.manager.status().paired);
}

#[tokio::test]
async fn service_stop_cancels_coordinator_before_manager_shutdown() {
    let f = Fixture::new().await;
    let _screen = f.screen().await;
    f.a.fail_connect.store(false, Ordering::Release);
    f.a.hold_connect_reply.store(true, Ordering::Release);
    f.admin
        .automatic
        .request(&f.admin, A.into(), f.admin.hub.phones.epoch());
    let running = tokio::spawn(f.admin.clone().run_standby(f.stop.subscribe()));
    f.a.connecting.notified().await;
    f.stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), running)
        .await
        .unwrap()
        .unwrap();
    f.admin.manager.stop().await;
    assert!(f.a.disconnects.load(Ordering::Acquire) > 0);
    assert!(f.admin.manager.status().paired);
}
