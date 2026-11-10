//! Targeted GATT discovery/notification probe. It sends no application payload and cannot
//! read battery/range/TPMS without the separate, vehicle-authorized Tesla protocol layer.
use super::*;
use zbus::zvariant::ObjectPath;

#[derive(Debug, serde::Serialize)]
pub struct GattReport {
    pub cancelled: bool,
    pub notifications: usize,
    pub complete_messages: usize,
}

pub(super) struct Characteristics {
    pub(super) rx: String,
    pub(super) tx: String,
}
pub(super) fn characteristics(objects: &Objects, device: &str) -> Result<Characteristics> {
    let text = |properties: &Properties, key: &str| {
        properties
            .get(key)
            .and_then(|v| <&str>::try_from(v).ok())
            .map(str::to_owned)
    };
    let object = |properties: &Properties, key: &str| {
        properties
            .get(key)
            .and_then(|v| <&ObjectPath<'_>>::try_from(v).ok())
            .map(ToString::to_string)
    };
    let services: Vec<_> = objects
        .iter()
        .filter(|(path, interfaces)| {
            path.as_str().starts_with(&format!("{device}/"))
                && interfaces.get("org.bluez.GattService1").is_some_and(|p| {
                    text(p, "UUID").is_some_and(|u| u.eq_ignore_ascii_case(SERVICE))
                        && object(p, "Device").as_deref() == Some(device)
                })
        })
        .collect();
    if services.len() != 1 {
        return Err("Tesla GATT service missing or ambiguous".into());
    }
    let service = services[0].0.as_str();
    let mut rx = Vec::new();
    let mut tx = Vec::new();
    for (path, interfaces) in objects {
        if !path.as_str().starts_with(&format!("{service}/")) {
            continue;
        }
        let Some(properties) = interfaces.get("org.bluez.GattCharacteristic1") else {
            continue;
        };
        if object(properties, "Service").as_deref() != Some(service) {
            continue;
        }
        let uuid = text(properties, "UUID").unwrap_or_default();
        let flags = properties
            .get("Flags")
            .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default();
        if uuid.eq_ignore_ascii_case(FROM_VEHICLE)
            && flags.iter().any(|f| f == "notify" || f == "indicate")
        {
            rx.push(path.to_string());
        }
        if uuid.eq_ignore_ascii_case(TO_VEHICLE) && flags.iter().any(|f| f == "write") {
            tx.push(path.to_string());
        }
    }
    if rx.len() != 1 || tx.len() != 1 {
        return Err("Tesla GATT characteristics missing or ambiguous".into());
    }
    Ok(Characteristics {
        rx: rx.remove(0),
        tx: tx.remove(0),
    })
}

/// The caller must select an authorized vehicle; a service UUID alone is not ownership.
/// No pairing, writes, retries or reconnections are performed. Await cancellation cleanup.
pub async fn observe_gatt(
    bus: &str,
    device: &str,
    duration: Duration,
    mut stop: watch::Receiver<bool>,
) -> Result<GattReport> {
    if !(Duration::from_secs(1)..=Duration::from_secs(60)).contains(&duration)
        || f50_server::phones::device_component(device).is_err()
    {
        return Err("invalid Tesla GATT probe target or duration".into());
    }
    if *stop.borrow() {
        return Err("Tesla GATT probe cancelled before connection".into());
    }
    let connection = tokio::select! {biased;
        _=stop.changed()=>return Err("Tesla GATT probe cancelled during bus connection".into()),
        result=connect(bus,Duration::from_secs(3))=>result?,
    };
    let owner = zbus::fdo::DBusProxy::new(&connection)
        .await?
        .get_name_owner(zbus::names::BusName::try_from("org.bluez")?)
        .await?
        .to_string();
    let result = observe_on(&connection, &owner, device, duration, stop).await;
    let closed = connection.close().await;
    let report = result?;
    closed?;
    Ok(report)
}

async fn observe_on(
    connection: &Connection,
    owner: &str,
    device: &str,
    duration: Duration,
    mut stop: watch::Receiver<bool>,
) -> Result<GattReport> {
    use zbus::export::futures_core::Stream;
    let target = Proxy::new(connection, owner, device, "org.bluez.Device1").await?;
    if target.get_property::<bool>("Connected").await? {
        return Err("Tesla GATT probe refuses an already connected target".into());
    }
    let uuids: Vec<String> = target.get_property("UUIDs").await?;
    if !uuids.iter().any(|u| u.eq_ignore_ascii_case(SERVICE)) {
        return Err("target does not advertise Tesla service".into());
    }
    if *stop.borrow() {
        return Err("Tesla GATT probe cancelled before device connection".into());
    }
    let mut notify: Option<Proxy<'_>> = None;
    let mut phase = "connect";
    // A Connect timeout can still leave a pending BlueZ attempt. From here onwards we own
    // that attempt and always cancel it, even when Connect/StartNotify fails or is cancelled.
    let run = async {
        tokio::select! {biased;
            _=stop.changed()=>return Err("Tesla GATT probe cancelled during connection".into()),
            result=target.call::<_,_,()>("Connect",&())=>result?,
        }
        phase = "discovery";
        tokio::time::timeout(Duration::from_secs(10),async {
            loop {
                if *stop.borrow() {return Err::<(),Box<dyn std::error::Error+Send+Sync>>("Tesla GATT probe cancelled during discovery".into());}
                if target.get_property::<bool>("ServicesResolved").await? {return Ok(());}
                tokio::select! {
                    _=stop.changed()=>return Err("Tesla GATT probe cancelled during discovery".into()),
                    _=tokio::time::sleep(Duration::from_millis(50))=>{},
                }
            }
        }).await??;
        let objects: Objects = Proxy::new(connection,owner,"/","org.freedesktop.DBus.ObjectManager")
            .await?.call("GetManagedObjects",&()).await?;
        let paths = characteristics(&objects,device)?;
        let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal)
            .sender(owner)?.path(paths.rx.as_str())?.interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?.build();
        let mut signals = zbus::MessageStream::for_match_rule(rule,connection,Some(64)).await?;
        phase = "subscription";
        // Store ownership before StartNotify so a lost reply still triggers StopNotify.
        notify = Some(Proxy::new(connection,owner,paths.rx,"org.bluez.GattCharacteristic1").await?);
        if *stop.borrow() {return Err("Tesla GATT probe cancelled before subscription".into());}
        notify.as_ref().unwrap().call::<_,_,()>("StartNotify",&()).await?;
        phase = "notifications";
        let mut frames = Frames::default();
        let mut report = GattReport {cancelled:false, notifications:0, complete_messages:0};
        let until = tokio::time::Instant::now()+duration;
        loop {
            let message = tokio::select! {biased;
                _=stop.changed()=>{report.cancelled=true;return Ok::<_,Box<dyn std::error::Error+Send+Sync>>(report);},
                _=tokio::time::sleep_until(until)=>return Ok(report),
                message=std::future::poll_fn(|cx|std::pin::Pin::new(&mut signals).poll_next(cx))=>message,
            };
            let message = message.ok_or("Tesla GATT notification stream ended")??;
            let (interface,mut changed,_): (String,Properties,Vec<String>)=message.body().deserialize()?;
            if interface!="org.bluez.GattCharacteristic1" {continue;}
            if let Some(value)=changed.remove("Value") {
                let chunk:Vec<u8>=value.try_into()?;
                report.notifications+=1;
                report.complete_messages+=frames.receive(&chunk,Instant::now())?.len();
            }
        }
    }.await;
    let unsubscribe = if let Some(notify) = notify {
        notify.call::<_, _, ()>("StopNotify", &()).await
    } else {
        Ok(())
    };
    let disconnect = target.call::<_, _, ()>("Disconnect", &()).await;
    // Cleanup failures must remain visible even when the primary operation also failed.
    if unsubscribe.is_err() || disconnect.is_err() {
        return Err(format!("Tesla GATT cleanup failed: phase={phase}, primary_failed={}, unsubscribe_ok={}, disconnect_ok={}",
            run.is_err(),unsubscribe.is_ok(),disconnect.is_ok()).into());
    }
    run
}

#[cfg(test)]
pub(super) mod tests;
