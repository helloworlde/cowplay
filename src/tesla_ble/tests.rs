use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
const OWNER: &str = ":1.42";
const PHONE: &str = "/org/bluez/hci0/dev_00_11_22_33_44_55";
#[derive(Default)]
struct Remote {
    discovering: AtomicBool,
    starts: AtomicUsize,
    stops: AtomicUsize,
    le_filter: AtomicBool,
    fail_stop: AtomicBool,
}
struct Adapter(Arc<Remote>);
#[zbus::interface(name = "org.bluez.Adapter1")]
impl Adapter {
    fn set_discovery_filter(&self, filter: HashMap<String, OwnedValue>) {
        self.0.le_filter.store(
            filter.len() == 1
                && filter
                    .get("Transport")
                    .and_then(|v| <&str>::try_from(v).ok())
                    == Some("le"),
            Ordering::SeqCst,
        );
    }
    async fn start_discovery(&self, #[zbus(connection)] c: &Connection) -> zbus::fdo::Result<()> {
        self.0.starts.fetch_add(1, Ordering::SeqCst);
        self.0.discovering.store(true, Ordering::SeqCst);
        let changed = HashMap::from([
            ("RSSI", Value::from(-62i16)),
            ("UUIDs", Value::from(vec![SERVICE])),
        ]);
        c.emit_signal(
            None::<&str>,
            PHONE,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &("org.bluez.Device1", changed, Vec::<String>::new()),
        )
        .await?;
        Ok(())
    }
    fn stop_discovery(&self) -> zbus::fdo::Result<()> {
        self.0.stops.fetch_add(1, Ordering::SeqCst);
        if self.0.fail_stop.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed("synthetic stop failure".into()));
        }
        self.0.discovering.store(false, Ordering::SeqCst);
        Ok(())
    }
    #[zbus(property)]
    fn powered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn discoverable(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn pairable(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn discovering(&self) -> bool {
        self.0.discovering.load(Ordering::SeqCst)
    }
}
struct Device;
#[zbus::interface(name = "org.bluez.Device1")]
impl Device {
    #[zbus(property)]
    fn connected(&self) -> bool {
        true
    }
}
async fn fixture() -> (Connection, Connection, Arc<Remote>) {
    let remote = Arc::new(Remote::default());
    let (a, b) = tokio::net::UnixStream::pair().unwrap();
    let server = zbus::connection::Builder::unix_stream(a)
        .server("0123456789abcdef0123456789abcdef")
        .unwrap()
        .p2p()
        .unique_name(OWNER)
        .unwrap()
        .serve_at("/", zbus::fdo::ObjectManager)
        .unwrap()
        .serve_at(ADAPTER, Adapter(remote.clone()))
        .unwrap()
        .serve_at(PHONE, Device)
        .unwrap()
        .build();
    let client = zbus::connection::Builder::unix_stream(b)
        .p2p()
        .unique_name(":1.43")
        .unwrap()
        .method_timeout(Duration::from_millis(200))
        .build();
    let (server, client) = tokio::try_join!(server, client).unwrap();
    (server, client, remote)
}
#[tokio::test]
async fn le_scan_observes_fresh_signals_and_releases_only_its_session() {
    let (_server, client, remote) = fixture().await;
    let (_stop, rx) = watch::channel(false);
    let report = scan_on(&client, OWNER, Duration::from_millis(40), rx)
        .await
        .unwrap();
    assert!(remote.le_filter.load(Ordering::SeqCst));
    assert_eq!(remote.starts.load(Ordering::SeqCst), 1);
    assert_eq!(remote.stops.load(Ordering::SeqCst), 1);
    assert_eq!(report.observed_devices, 1);
    assert_eq!(report.tesla_service_devices, 1);
    assert_eq!(report.before, report.after);
    assert!(report.during.discovering);
    assert_eq!(report.before.connected_devices, 1);
    assert!(!report.cancelled);
    let public = serde_json::to_string(&report).unwrap();
    assert!(!public.contains(PHONE));
    assert!(!public.contains("00_11_22"));
}
#[tokio::test]
async fn cancellation_stops_scan_and_allows_next_scan() {
    let (_server, client, remote) = fixture().await;
    let (stop, rx) = watch::channel(false);
    let scan = scan_on(&client, OWNER, Duration::from_secs(60), rx);
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        stop.send(true).unwrap();
    };
    let (report, ()) = tokio::join!(scan, cancel);
    assert!(report.unwrap().cancelled);
    let (_stop, rx) = watch::channel(false);
    assert!(
        !scan_on(&client, OWNER, Duration::from_millis(10), rx)
            .await
            .unwrap()
            .cancelled
    );
    assert_eq!(remote.stops.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn existing_discovery_and_pre_cancel_do_not_touch_adapter() {
    let (_server, client, remote) = fixture().await;
    let (_stop, rx) = watch::channel(false);
    remote.discovering.store(true, Ordering::SeqCst);
    assert!(
        scan_on(&client, OWNER, Duration::from_millis(10), rx)
            .await
            .is_err()
    );
    remote.discovering.store(false, Ordering::SeqCst);
    let (_stop, rx) = watch::channel(true);
    assert!(
        scan_on(&client, OWNER, Duration::from_millis(10), rx)
            .await
            .is_err()
    );
    assert_eq!(remote.starts.load(Ordering::SeqCst), 0);
    assert_eq!(remote.stops.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn cleanup_failure_is_not_reported_as_success() {
    let (_server, client, remote) = fixture().await;
    remote.fail_stop.store(true, Ordering::SeqCst);
    let (_stop, rx) = watch::channel(false);
    assert!(
        scan_on(&client, OWNER, Duration::from_millis(10), rx)
            .await
            .is_err()
    );
    assert_eq!(remote.stops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stalled_bus_handshake_is_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("synthetic-bus");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let address = format!("unix:path={}", socket.display());
    let start = Instant::now();
    let (result, accepted) = tokio::join!(
        connect(&address, Duration::from_millis(30)),
        listener.accept()
    );
    let _accepted = accepted.unwrap();
    assert!(result.is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn cancellation_interrupts_stalled_bus_handshake() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("synthetic-bus");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let address = format!("unix:path={}", socket.display());
    let (stop, rx) = watch::channel(false);
    let cancel = async {
        let _accepted = listener.accept().await.unwrap();
        stop.send(true).unwrap();
        // Keep the accepted socket alive until the cancelled client returns.
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    let (result, ()) = tokio::join!(scan(&address, Duration::from_secs(60), rx), cancel);
    assert!(result.unwrap_err().to_string().contains("cancelled"));
}
