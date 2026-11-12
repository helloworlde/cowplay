//! Tesla BLE read-only vehicle integration and bounded transport diagnostics.
//! The discovery client owns a separate private-bus connection; never reuse the iAP2 client's
//! discovery session or change adapter power, advertising, identity or pairability.
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use zbus::{
    Connection, Proxy,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

pub const SERVICE: &str = "00000211-b2d1-43f0-9b88-960cebf8b91e";
pub const TO_VEHICLE: &str = "00000212-b2d1-43f0-9b88-960cebf8b91e";
pub const FROM_VEHICLE: &str = "00000213-b2d1-43f0-9b88-960cebf8b91e";
const ADAPTER: &str = "/org/bluez/hci0";
type Properties = HashMap<String, OwnedValue>;
type Interfaces = HashMap<String, Properties>;
type Objects = HashMap<OwnedObjectPath, Interfaces>;
type Result<T> = crate::RuntimeResult<T>;

mod wire;
pub use wire::Frames;
mod gatt;
pub use gatt::{GattReport, observe_gatt};
pub mod transport;
pub mod worker;
pub mod management;
mod climate;

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct AdapterState {
    pub powered: bool,
    pub discoverable: bool,
    pub pairable: bool,
    pub discovering: bool,
    pub connected_devices: usize,
}
#[derive(Debug, serde::Serialize)]
pub struct ScanReport {
    pub elapsed_ms: u128,
    pub cancelled: bool,
    /// Fresh discovery signals only; not the daemon's cached device list.
    pub observed_devices: usize,
    /// Service advertisements are not VIN identification or vehicle authorization.
    pub tesla_service_devices: usize,
    pub before: AdapterState,
    pub during: AdapterState,
    pub after: AdapterState,
}

async fn state(connection: &Connection, owner: &str) -> Result<AdapterState> {
    let adapter = Proxy::new(connection, owner, ADAPTER, "org.bluez.Adapter1").await?;
    let objects: Objects = Proxy::new(connection, owner, "/", "org.freedesktop.DBus.ObjectManager")
        .await?
        .call("GetManagedObjects", &())
        .await?;
    let connected_devices = objects
        .values()
        .filter_map(|i| i.get("org.bluez.Device1"))
        .filter(|p| p.get("Connected").and_then(|v| bool::try_from(v).ok()) == Some(true))
        .count();
    Ok(AdapterState {
        powered: adapter.get_property("Powered").await?,
        discoverable: adapter.get_property("Discoverable").await?,
        pairable: adapter.get_property("Pairable").await?,
        discovering: adapter.get_property("Discovering").await?,
        connected_devices,
    })
}

/// Bounded LE scan on the already running private BlueZ. This does not start BlueZ or pair.
/// Callers must await completion after signalling cancellation, rather than aborting the task.
pub async fn scan(
    bus: &str,
    duration: Duration,
    mut stop: watch::Receiver<bool>,
) -> Result<ScanReport> {
    if !(Duration::from_secs(1)..=Duration::from_secs(60)).contains(&duration) {
        return Err("BLE scan duration must be 1..60 seconds".into());
    }
    if *stop.borrow() {
        return Err("BLE scan cancelled before bus connection".into());
    }
    let connection = tokio::select! { biased;
        _ = stop.changed() => return Err("BLE scan cancelled during bus connection".into()),
        result = connect(bus, Duration::from_secs(3)) => result?,
    };
    let owner: String = zbus::fdo::DBusProxy::new(&connection)
        .await?
        .get_name_owner(zbus::names::BusName::try_from("org.bluez")?)
        .await?
        .to_string();
    let result = scan_on(&connection, &owner, duration, stop).await;
    // Closing the dedicated client also releases its discovery filter, including failures
    // or a StartDiscovery response lost to a timeout. It cannot own iAP2 registrations.
    let closed = connection.close().await;
    let report = result?;
    closed?;
    Ok(report)
}

async fn connect(bus: &str, timeout: Duration) -> Result<Connection> {
    // method_timeout bounds calls after authentication, not the SASL handshake itself.
    Ok(tokio::time::timeout(
        timeout,
        zbus::connection::Builder::address(bus)?
            .method_timeout(Duration::from_secs(3))
            .build(),
    )
    .await??)
}

async fn scan_on(
    connection: &Connection,
    owner: &str,
    duration: Duration,
    mut stop: watch::Receiver<bool>,
) -> Result<ScanReport> {
    use zbus::export::futures_core::Stream;
    let before = state(connection, owner).await?;
    if !before.powered || before.discovering {
        return Err("BLE scan requires a powered adapter with no existing discovery".into());
    }
    if *stop.borrow() {
        return Err("BLE scan cancelled before start".into());
    }
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(owner)?
        .build();
    let mut signals = zbus::MessageStream::for_match_rule(rule, connection, Some(256)).await?;
    let adapter = Proxy::new(connection, owner, ADAPTER, "org.bluez.Adapter1").await?;
    let filter = HashMap::from([("Transport", Value::from("le"))]);
    adapter
        .call::<_, _, ()>("SetDiscoveryFilter", &(filter,))
        .await?;
    if *stop.borrow() {
        return Err("BLE scan cancelled before discovery".into());
    }
    let started = Instant::now();
    adapter.call::<_, _, ()>("StartDiscovery", &()).await?;
    let mut seen = HashSet::new();
    let mut tesla = HashSet::new();
    let until = tokio::time::Instant::now() + duration;
    let run = async {
        let during = state(connection, owner).await?;
        loop {
            let message = tokio::select! { biased;
                _ = stop.changed() => return Ok::<_, Box<dyn std::error::Error + Send + Sync>>((true,during)),
                _ = tokio::time::sleep_until(until) => return Ok((false,during)),
                message = std::future::poll_fn(|cx| std::pin::Pin::new(&mut signals).poll_next(cx)) => message,
            };
            let message = message.ok_or("BlueZ signal stream ended during scan")??;
            let header = message.header();
            let observation = match (header.interface().map(|s|s.as_str()), header.member().map(|s|s.as_str())) {
                (Some("org.freedesktop.DBus.ObjectManager"), Some("InterfacesAdded")) => {
                    message.body().deserialize::<(OwnedObjectPath, Interfaces)>().ok()
                        .and_then(|(path, mut i)| i.remove("org.bluez.Device1").map(|p|(path.to_string(),p)))
                },
                (Some("org.freedesktop.DBus.Properties"), Some("PropertiesChanged")) => {
                    message.body().deserialize::<(String, Properties, Vec<String>)>().ok()
                        .filter(|(i,_,_)| i == "org.bluez.Device1")
                        .and_then(|(_,p,_)| header.path().map(|path|(path.to_string(),p)))
                },
                _ => None,
            };
            if let Some((path, properties)) = observation {
                if !path.starts_with("/org/bluez/hci0/dev_") { continue; }
                // RSSI/advertisement changes, not a cached Connected/ServicesResolved signal.
                if properties.keys().any(|p| matches!(p.as_str(), "RSSI"|"ManufacturerData"|"ServiceData")) {
                    seen.insert(path.clone());
                }
                if properties.get("UUIDs").and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
                    .is_some_and(|uuids| uuids.iter().any(|u|u.eq_ignore_ascii_case(SERVICE))) {
                    tesla.insert(path);
                }
            }
        }
    }.await;
    // Always release just this client's session before returning a receive error.
    let cleanup = adapter.call::<_, _, ()>("StopDiscovery", &()).await;
    let (cancelled, during) = run?;
    cleanup?;
    let after = state(connection, owner).await?;
    Ok(ScanReport {
        elapsed_ms: started.elapsed().as_millis(),
        cancelled,
        observed_devices: seen.len(),
        tesla_service_devices: tesla.len(),
        before,
        during,
        after,
    })
}

#[cfg(test)]
mod tests;
