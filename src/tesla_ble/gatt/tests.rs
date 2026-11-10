use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
const OWNER: &str = ":1.42";
const DEVICE: &str = "/org/bluez/hci0/dev_00_11_22_33_44_66";
const SERVICE_PATH: &str = "/org/bluez/hci0/dev_00_11_22_33_44_66/service001";
const RX: &str = "/org/bluez/hci0/dev_00_11_22_33_44_66/service001/char001";
const TX: &str = "/org/bluez/hci0/dev_00_11_22_33_44_66/service001/char002";
#[derive(Default)]
pub(in crate::tesla_ble) struct Remote {
    pub(in crate::tesla_ble) connected: AtomicBool,
    pub(in crate::tesla_ble) disconnects: AtomicUsize,
    pub(in crate::tesla_ble) subscribes: AtomicUsize,
    pub(in crate::tesla_ble) unsubscribes: AtomicUsize,
    fail_subscribe: AtomicBool,
    fail_connect: AtomicBool,
    fail_cleanup: AtomicBool,
    hang_connect: AtomicBool,
    pub(in crate::tesla_ble) writes: std::sync::Mutex<Vec<Vec<u8>>>,
}
struct Device(Arc<Remote>);
#[zbus::interface(name = "org.bluez.Device1")]
impl Device {
    #[zbus(property)]
    fn name(&self) -> &str {
        "S0000000000000000C"
    }
    async fn connect(&self, #[zbus(connection)] c: &Connection) -> zbus::fdo::Result<()> {
        self.0.connected.store(true, Ordering::SeqCst);
        c.emit_signal(
            None::<&str>,
            DEVICE,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(
                "org.bluez.Device1",
                HashMap::from([
                    ("Connected", Value::from(true)),
                    ("ServicesResolved", Value::from(true)),
                ]),
                Vec::<String>::new(),
            ),
        )
        .await?;
        if self.0.hang_connect.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        if self.0.fail_connect.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed(
                "synthetic failed Connect after side effect".into(),
            ));
        }
        Ok(())
    }
    fn disconnect(&self) -> zbus::fdo::Result<()> {
        self.0.disconnects.fetch_add(1, Ordering::SeqCst);
        if self.0.fail_cleanup.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed(
                "synthetic disconnect failure".into(),
            ));
        }
        self.0.connected.store(false, Ordering::SeqCst);
        Ok(())
    }
    #[zbus(property)]
    fn connected(&self) -> bool {
        self.0.connected.load(Ordering::SeqCst)
    }
    #[zbus(property)]
    fn services_resolved(&self) -> bool {
        self.0.connected.load(Ordering::SeqCst)
    }
    #[zbus(property, name = "UUIDs")]
    fn uuids(&self) -> Vec<String> {
        vec![SERVICE.into()]
    }
}
struct Service;
#[zbus::interface(name = "org.bluez.GattService1")]
impl Service {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        SERVICE
    }
    #[zbus(property)]
    fn device(&self) -> ObjectPath<'_> {
        ObjectPath::try_from(DEVICE).unwrap()
    }
}
struct Characteristic(Arc<Remote>, bool);
#[zbus::interface(name = "org.bluez.GattCharacteristic1")]
impl Characteristic {
    fn write_value(
        &self,
        bytes: Vec<u8>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        if options.get("type").and_then(|v| <&str>::try_from(v).ok()) != Some("request") {
            return Err(zbus::fdo::Error::Failed(
                "expected acknowledged GATT write".into(),
            ));
        }
        self.0.writes.lock().unwrap().push(bytes);
        Ok(())
    }
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        if self.1 { FROM_VEHICLE } else { TO_VEHICLE }
    }
    #[zbus(property)]
    fn service(&self) -> ObjectPath<'_> {
        ObjectPath::try_from(SERVICE_PATH).unwrap()
    }
    #[zbus(property)]
    fn flags(&self) -> Vec<String> {
        vec![if self.1 {
            "notify".into()
        } else {
            "write".into()
        }]
    }
    async fn start_notify(&self, #[zbus(connection)] c: &Connection) -> zbus::fdo::Result<()> {
        self.0.subscribes.fetch_add(1, Ordering::SeqCst);
        if self.0.fail_subscribe.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed(
                "synthetic subscribe failure".into(),
            ));
        }
        for bytes in [vec![0u8], vec![2, 8, 9, 0, 1, 7]] {
            let changed = HashMap::from([("Value", Value::from(bytes))]);
            c.emit_signal(
                None::<&str>,
                RX,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &(
                    "org.bluez.GattCharacteristic1",
                    changed,
                    Vec::<String>::new(),
                ),
            )
            .await?;
        }
        Ok(())
    }
    fn stop_notify(&self) {
        self.0.unsubscribes.fetch_add(1, Ordering::SeqCst);
    }
}
pub(in crate::tesla_ble) async fn fixture() -> (Connection, Connection, Arc<Remote>) {
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
        .serve_at(ADAPTER, Adapter::default())
        .unwrap()
        .serve_at(DEVICE, Device(remote.clone()))
        .unwrap()
        .serve_at(SERVICE_PATH, Service)
        .unwrap()
        .serve_at(RX, Characteristic(remote.clone(), true))
        .unwrap()
        .serve_at(TX, Characteristic(remote.clone(), false))
        .unwrap()
        .build();
    let client = zbus::connection::Builder::unix_stream(b)
        .p2p()
        .unique_name(":1.43")
        .unwrap()
        .method_timeout(Duration::from_millis(100))
        .build();
    let (server, client) = tokio::try_join!(server, client).unwrap();
    (server, client, remote)
}
#[tokio::test]
async fn gatt_discovers_subscribes_reassembles_and_cleans_up() {
    let (_s, c, r) = fixture().await;
    let (_stop, rx) = watch::channel(false);
    let report = observe_on(&c, OWNER, DEVICE, Duration::from_millis(30), rx)
        .await
        .unwrap();
    assert_eq!(report.notifications, 2);
    assert_eq!(report.complete_messages, 2);
    assert!(!report.cancelled);
    assert!(!r.connected.load(Ordering::SeqCst));
    assert_eq!(r.subscribes.load(Ordering::SeqCst), 1);
    assert_eq!(r.unsubscribes.load(Ordering::SeqCst), 1);
    assert_eq!(r.disconnects.load(Ordering::SeqCst), 1);
    assert!(!serde_json::to_string(&report).unwrap().contains(DEVICE));
}
#[tokio::test]
async fn partial_connect_and_subscription_failures_are_cleaned_up() {
    for fail_connect in [true, false] {
        let (_s, c, r) = fixture().await;
        let (_stop, rx) = watch::channel(false);
        r.fail_connect.store(fail_connect, Ordering::SeqCst);
        r.fail_subscribe.store(!fail_connect, Ordering::SeqCst);
        assert!(
            observe_on(&c, OWNER, DEVICE, Duration::from_millis(30), rx)
                .await
                .is_err()
        );
        assert!(!r.connected.load(Ordering::SeqCst));
        assert_eq!(r.disconnects.load(Ordering::SeqCst), 1);
        assert_eq!(
            r.unsubscribes.load(Ordering::SeqCst),
            usize::from(!fail_connect)
        );
    }
}
#[tokio::test]
async fn connected_target_is_never_disconnected_by_probe() {
    let (_s, c, r) = fixture().await;
    let (_stop, rx) = watch::channel(false);
    r.connected.store(true, Ordering::SeqCst);
    assert!(
        observe_on(&c, OWNER, DEVICE, Duration::from_millis(30), rx)
            .await
            .is_err()
    );
    assert_eq!(r.disconnects.load(Ordering::SeqCst), 0);
    assert_eq!(r.subscribes.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn cancellation_releases_subscription_and_connection() {
    let (_s, c, r) = fixture().await;
    let (stop, rx) = watch::channel(false);
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        stop.send(true).unwrap();
    };
    let (report, ()) = tokio::join!(
        observe_on(&c, OWNER, DEVICE, Duration::from_secs(60), rx),
        cancel
    );
    assert!(report.unwrap().cancelled);
    assert_eq!(r.unsubscribes.load(Ordering::SeqCst), 1);
    assert_eq!(r.disconnects.load(Ordering::SeqCst), 1);
    assert!(!r.connected.load(Ordering::SeqCst));
}
#[tokio::test]
async fn cleanup_failure_cannot_be_a_successful_probe() {
    let (_s, c, r) = fixture().await;
    let (_stop, rx) = watch::channel(false);
    r.fail_cleanup.store(true, Ordering::SeqCst);
    let error = observe_on(&c, OWNER, DEVICE, Duration::from_millis(10), rx)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cleanup failed"));
}
#[tokio::test]
async fn timed_out_and_cancelled_connect_attempts_are_disconnected() {
    for cancel in [false, true] {
        let (_s, c, r) = fixture().await;
        let (stop, rx) = watch::channel(false);
        r.hang_connect.store(true, Ordering::SeqCst);
        let trigger = async {
            if cancel {
                tokio::time::sleep(Duration::from_millis(20)).await;
                stop.send(true).unwrap();
            }
        };
        let (result, ()) = tokio::join!(
            observe_on(&c, OWNER, DEVICE, Duration::from_secs(60), rx),
            trigger
        );
        assert!(result.is_err());
        assert_eq!(r.disconnects.load(Ordering::SeqCst), 1);
        assert!(!r.connected.load(Ordering::SeqCst));
        assert_eq!(r.subscribes.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn primary_phase_is_preserved_when_cleanup_also_fails() {
    let (_s, c, r) = fixture().await;
    let (_stop, rx) = watch::channel(false);
    r.fail_subscribe.store(true, Ordering::SeqCst);
    r.fail_cleanup.store(true, Ordering::SeqCst);
    let error = observe_on(&c, OWNER, DEVICE, Duration::from_millis(10), rx)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("phase=subscription, primary_failed=true"));
    assert!(error.contains("unsubscribe_ok=true, disconnect_ok=false"));
}
#[tokio::test]
async fn characteristics_must_belong_to_one_service_of_exact_target() {
    let (_s, c, _r) = fixture().await;
    let objects: Objects = Proxy::new(&c, OWNER, "/", "org.freedesktop.DBus.ObjectManager")
        .await
        .unwrap()
        .call("GetManagedObjects", &())
        .await
        .unwrap();
    assert_eq!(characteristics(&objects, DEVICE).unwrap().rx, RX);
    assert!(characteristics(&objects, "/org/bluez/hci0/dev_00_11_22_33_44_77").is_err());
    let mut moved = objects
        .iter()
        .map(|(path, interfaces)| {
            (
                path.clone(),
                interfaces
                    .iter()
                    .map(|(i, p)| {
                        (
                            i.clone(),
                            p.iter()
                                .map(|(k, v)| (k.clone(), v.try_clone().unwrap()))
                                .collect(),
                        )
                    })
                    .collect(),
            )
        })
        .collect::<Objects>();
    let impostor = moved
        .remove(&OwnedObjectPath::try_from(TX).unwrap())
        .unwrap();
    moved.insert(
        OwnedObjectPath::try_from("/org/bluez/hci0/dev_00_11_22_33_44_77/service001/char002")
            .unwrap(),
        impostor,
    );
    assert!(
        characteristics(&moved, DEVICE).is_err(),
        "unrelated device's TX cannot complete target service"
    );
    let mut duplicate = objects;
    let properties = duplicate
        .get(&OwnedObjectPath::try_from(RX).unwrap())
        .unwrap()
        .iter()
        .map(|(i, p)| {
            (
                i.clone(),
                p.iter()
                    .map(|(k, v)| (k.clone(), v.try_clone().unwrap()))
                    .collect(),
            )
        })
        .collect();
    duplicate.insert(
        OwnedObjectPath::try_from(format!("{SERVICE_PATH}/char003")).unwrap(),
        properties,
    );
    assert!(
        characteristics(&duplicate, DEVICE).is_err(),
        "ambiguous RX must be rejected"
    );
}

#[derive(Default)]
struct Adapter {
    discovering: AtomicBool,
}
#[zbus::interface(name = "org.bluez.Adapter1")]
impl Adapter {
    #[zbus(property)]
    fn powered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn discovering(&self) -> bool {
        self.discovering.load(Ordering::SeqCst)
    }
    fn set_discovery_filter(&self, _filter: HashMap<String, OwnedValue>) {}
    async fn start_discovery(&self, #[zbus(connection)] c: &Connection) -> zbus::fdo::Result<()> {
        self.discovering.store(true, Ordering::SeqCst);
        c.emit_signal(
            None::<&str>,
            DEVICE,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(
                "org.bluez.Device1",
                HashMap::from([("RSSI", Value::from(-50i16))]),
                Vec::<String>::new(),
            ),
        )
        .await?;
        Ok(())
    }
    fn stop_discovery(&self) {
        self.discovering.store(false, Ordering::SeqCst);
    }
}
