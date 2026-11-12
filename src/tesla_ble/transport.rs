//! A single owned Tesla GATT connection on the existing private BlueZ bus.
use super::*;
use std::collections::VecDeque;
use zbus::export::futures_core::Stream;
#[derive(Debug)]
pub(super) struct Failure {
    pub(super) stage: &'static str,
    pub(super) cleanup_uncertain: bool,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tesla BLE {}", self.stage)
    }
}
impl std::error::Error for Failure {}

pub struct Link {
    bus: Connection,
    owner: String,
    adapter: String,
    device: String,
    rx: String,
    tx: String,
    signals: zbus::MessageStream,
    frames: Frames,
    ready: VecDeque<Vec<u8>>,
    connected: bool,
    notifying: bool,
    stage: &'static str,
}
async fn next(stream: &mut zbus::MessageStream) -> Option<zbus::Result<zbus::Message>> {
    std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx)).await
}
async fn objects(bus: &Connection, owner: &str) -> Result<Objects> {
    Ok(
        Proxy::new(bus, owner, "/", "org.freedesktop.DBus.ObjectManager")
            .await?
            .call("GetManagedObjects", &())
            .await?,
    )
}
impl Link {
    /// Discovery is finite and matches Tesla's VIN-derived local name, not a generic UUID.
    pub async fn open(address: &str, name: &str, stop: watch::Receiver<bool>) -> Result<Self> {
        Self::open_on(address, name, stop, 0).await
    }
    pub async fn open_on(address: &str, name: &str, mut stop: watch::Receiver<bool>, adapter: u16) -> Result<Self> {
        if name.len() != 18
            || !name.starts_with('S')
            || !name.ends_with('C')
            || !name.as_bytes()[1..17].iter().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid Tesla local name".into());
        }
        if *stop.borrow() {
            return Err("Tesla BLE cancelled before connection".into());
        }
        let bus = tokio::select! {biased;_=stop.changed()=>return Err("Tesla BLE cancelled".into()),r=connect(address,Duration::from_secs(3))=>r?};
        let owner = zbus::fdo::DBusProxy::new(&bus)
            .await?
            .get_name_owner(zbus::names::BusName::try_from("org.bluez")?)
            .await?
            .to_string();
        Self::using_adapter(bus, owner, name, &mut stop, adapter).await
    }
    #[cfg(test)]
    pub(in crate::tesla_ble) async fn using_bus(bus: Connection, owner: String, name: &str, stop: &mut watch::Receiver<bool>) -> Result<Self> {
        Self::using_adapter(bus, owner, name, stop, 0).await
    }
    async fn using_adapter(bus: Connection, owner: String, name: &str, stop: &mut watch::Receiver<bool>, adapter: u16) -> Result<Self> {
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(owner.as_str())?
            .build();
        let signals = zbus::MessageStream::for_match_rule(rule, &bus, Some(128)).await?;
        let mut link = Self {
            bus,
            owner,
            adapter: format!("/org/bluez/hci{adapter}"),
            device: String::new(),
            rx: String::new(),
            tx: String::new(),
            signals,
            frames: Frames::default(),
            ready: VecDeque::new(),
            connected: false,
            notifying: false,
            stage: "ble_scan_failed",
        };
        if let Err(error) = link.prepare(name, stop).await {
            let cleanup = link.close().await;
            if cleanup.is_err() {
                return Err(Box::new(Failure {
                    stage: "cleanup_failed",
                    cleanup_uncertain: true,
                }));
            }
            let _ = error;
            return Err(Box::new(Failure {
                stage: link.stage,
                cleanup_uncertain: false,
            }));
        }
        Ok(link)
    }
    async fn proxy<'a>(&'a self, path: &'a str, interface: &'a str) -> zbus::Result<Proxy<'a>> {
        Proxy::new(&self.bus, self.owner.as_str(), path, interface).await
    }
    async fn prepare(&mut self, name: &str, stop: &mut watch::Receiver<bool>) -> Result<()> {
        let adapter = self.proxy(&self.adapter, "org.bluez.Adapter1").await?;
        if !adapter.get_property::<bool>("Powered").await?
            || adapter.get_property::<bool>("Discovering").await?
        {
            return Err("Tesla BLE requires powered adapter without another discovery".into());
        }
        adapter
            .call::<_, _, ()>(
                "SetDiscoveryFilter",
                &(HashMap::from([
                    ("Transport", Value::from("le")),
                    ("Pattern", Value::from(name)),
                ]),),
            )
            .await?;
        if *stop.borrow() {
            return Err("Tesla BLE cancelled before scan".into());
        }
        adapter.call::<_, _, ()>("StartDiscovery", &()).await?;
        drop(adapter);
        let scan=async {
            // The pattern filter reduces radio traffic. BlueZ merges filters, so still verify names.
            let until=tokio::time::Instant::now()+Duration::from_secs(15);
            loop {
                let message=tokio::select!{biased;_=stop.changed()=>return Err::<String,Box<dyn std::error::Error+Send+Sync>>("Tesla BLE discovery cancelled".into()),_=tokio::time::sleep_until(until)=>return Err("Tesla BLE target not found".into()),m=next(&mut self.signals)=>m};
                let message=message.ok_or("Tesla BLE discovery stream ended")??;
                let header=message.header();
                let path=match (header.interface().map(|s|s.as_str()),header.member().map(|s|s.as_str())){
                    (Some("org.freedesktop.DBus.ObjectManager"),Some("InterfacesAdded"))=>message.body().deserialize::<(OwnedObjectPath,Interfaces)>().ok().filter(|(_,i)|i.contains_key("org.bluez.Device1")).map(|(p,_)|p.to_string()),
                    (Some("org.freedesktop.DBus.Properties"),Some("PropertiesChanged"))=>message.body().deserialize::<(String,Properties,Vec<String>)>().ok().filter(|(i,p,_)|i=="org.bluez.Device1"&&p.keys().any(|k|matches!(k.as_str(),"RSSI"|"Name"|"ManufacturerData"|"ServiceData"))).and_then(|_|header.path().map(ToString::to_string)),
                    _=>None,
                };
                if let Some(path)=path.filter(|p|p.starts_with(&format!("{}/dev_",self.adapter))&&f50_server::phones::device_component(p).is_ok()){
                    let device=Proxy::new(&self.bus,self.owner.as_str(),path.as_str(),"org.bluez.Device1").await?;
                    if device.get_property::<String>("Name").await.ok().as_deref()==Some(name){return Ok(path);}
                }
            }
        }.await;
        let cleanup = self
            .proxy(&self.adapter, "org.bluez.Adapter1")
            .await?
            .call::<_, _, ()>("StopDiscovery", &())
            .await;
        self.device = scan?;
        cleanup?;
        self.stage = "ble_connect_failed";
        let target = self.proxy(&self.device, "org.bluez.Device1").await?;
        if target.get_property::<bool>("Connected").await? {
            return Err("Tesla BLE target already owned by another connection".into());
        }
        drop(target);
        if *stop.borrow() {
            return Err("Tesla BLE cancelled before connect".into());
        }
        self.connected = true; // Own the pending attempt even if Connect's reply is lost.
        let target = self.proxy(&self.device, "org.bluez.Device1").await?;
        tokio::select! {biased;_=stop.changed()=>return Err("Tesla BLE connect cancelled".into()),r=target.call::<_,_,()>("Connect",&())=>r?};
        drop(target);
        self.stage = "ble_gatt_failed";
        tokio::time::timeout(Duration::from_secs(10),async{
            loop {
                if *stop.borrow(){return Err::<(),Box<dyn std::error::Error+Send+Sync>>("Tesla BLE services cancelled".into());}
                if self.proxy(&self.device,"org.bluez.Device1").await?.get_property::<bool>("ServicesResolved").await?{return Ok(());}
                tokio::select!{_=stop.changed()=>return Err("Tesla BLE services cancelled".into()),_=tokio::time::sleep(Duration::from_millis(50))=>{}}
            }
        }).await??;
        let paths =
            super::gatt::characteristics(&objects(&self.bus, &self.owner).await?, &self.device)?;
        self.rx = paths.rx;
        self.tx = paths.tx;
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(self.owner.as_str())?
            .interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?
            .path_namespace(self.device.as_str())?
            .build();
        self.signals = zbus::MessageStream::for_match_rule(rule, &self.bus, Some(64)).await?;
        if *stop.borrow() {
            return Err("Tesla BLE cancelled before subscription".into());
        }
        self.notifying = true;
        self.proxy(&self.rx, "org.bluez.GattCharacteristic1")
            .await?
            .call::<_, _, ()>("StartNotify", &())
            .await?;
        Ok(())
    }
    pub async fn send(&self, message: &[u8], stop: &mut watch::Receiver<bool>) -> Result<()> {
        let tx = self
            .proxy(&self.tx, "org.bluez.GattCharacteristic1")
            .await?;
        for bytes in Frames::chunks(message, 20)? {
            if *stop.borrow() {
                return Err("Tesla BLE write cancelled".into());
            }
            let options = HashMap::from([("type", Value::from("request"))]);
            let args = (bytes, options);
            tokio::select! {biased;_=stop.changed()=>return Err("Tesla BLE write cancelled".into()),r=tx.call::<_,_,()>("WriteValue",&args)=>r?}
        }
        Ok(())
    }
    pub async fn receive(&mut self, stop: &mut watch::Receiver<bool>) -> Result<Vec<u8>> {
        loop {
            if let Some(packet) = self.ready.pop_front() {
                return Ok(packet);
            }
            let message = tokio::select! {biased;_=stop.changed()=>return Err("Tesla BLE read cancelled".into()),m=next(&mut self.signals)=>m};
            let message = message.ok_or("Tesla BLE stream ended")??;
            let header = message.header();
            if header.interface().map(|s| s.as_str()) != Some("org.freedesktop.DBus.Properties")
                || header.member().map(|s| s.as_str()) != Some("PropertiesChanged")
            {
                continue;
            }
            let (interface, mut changed, invalidated): (String, Properties, Vec<String>) =
                message.body().deserialize()?;
            if header.path().map(|p| p.as_str()) == Some(&self.device)
                && interface == "org.bluez.Device1"
            {
                if changed
                    .get("Connected")
                    .and_then(|v| bool::try_from(v).ok())
                    == Some(false)
                    || invalidated.iter().any(|s| s == "Connected")
                {
                    return Err("Tesla BLE target disconnected".into());
                }
            }
            if header.path().map(|p| p.as_str()) == Some(&self.rx)
                && interface == "org.bluez.GattCharacteristic1"
            {
                if let Some(value) = changed.remove("Value") {
                    let chunk: Vec<u8> = value.try_into()?;
                    let packets = self.frames.receive(&chunk, Instant::now())?;
                    if self.ready.len() + packets.len() > 16 {
                        return Err("Tesla BLE receive queue full".into());
                    }
                    self.ready.extend(packets);
                }
            }
        }
    }
    pub async fn close(&mut self) -> Result<()> {
        let mut failed = false;
        if self.notifying {
            self.notifying = false;
            failed |= self
                .proxy(&self.rx, "org.bluez.GattCharacteristic1")
                .await?
                .call::<_, _, ()>("StopNotify", &())
                .await
                .is_err();
        }
        if self.connected {
            self.connected = false;
            failed |= self
                .proxy(&self.device, "org.bluez.Device1")
                .await?
                .call::<_, _, ()>("Disconnect", &())
                .await
                .is_err();
        }
        // A separate client owns this connection and no CowPlay/iAP2 registrations.
        failed |= self.bus.clone().close().await.is_err();
        if failed {
            return Err(Box::new(Failure {
                stage: "cleanup_failed",
                cleanup_uncertain: true,
            }));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    #[tokio::test]
    async fn actual_gatt_transport_discovers_writes_receives_and_releases_its_target() {
        let (_server, bus, remote) = super::super::gatt::tests::fixture().await;
        let (_stop, mut rx) = watch::channel(false);
        let mut link = Link::using_bus(bus, ":1.42".into(), "S0000000000000000C", &mut rx)
            .await
            .unwrap();
        link.send(&[7; 67], &mut rx).await.unwrap();
        let writes = remote.writes.lock().unwrap().clone();
        assert!(writes.iter().all(|w| w.len() <= 20));
        assert_eq!(
            writes.concat(),
            Frames::chunks(&[7; 67], 20).unwrap().concat()
        );
        assert_eq!(link.receive(&mut rx).await.unwrap(), vec![8, 9]);
        assert_eq!(link.receive(&mut rx).await.unwrap(), vec![7]);
        link.close().await.unwrap();
        assert!(!remote.connected.load(Ordering::SeqCst));
        assert_eq!(remote.unsubscribes.load(Ordering::SeqCst), 1);
        assert_eq!(remote.disconnects.load(Ordering::SeqCst), 1);
    }
}
