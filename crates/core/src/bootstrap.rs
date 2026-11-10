//! Owns link -> CSM -> wireless authentication, without I/O or platform references.
use crate::{
    framing::CsmDecoder,
    link::{Event, Link},
    mfi::Identity,
    wireless::{Endpoint, Stage, Wireless},
    Error, Result,
};
pub struct Bootstrap {
    link: Link,
    csm: CsmDecoder,
    wireless: Option<Wireless>,
    vehicle: Option<crate::vehicle_status::VehicleStatus>,
}
impl Bootstrap {
    pub fn new(tunnel: bool, now: u64) -> Result<Self> {
        let mut link = Link::default();
        if tunnel {
            link.start_tunnel(now)?
        } else {
            link.start(now)?
        }
        Ok(Self {
            link,
            csm: CsmDecoder::default(),
            wireless: None,
            vehicle: None,
        })
    }
    pub fn output(&mut self) -> Vec<u8> {
        self.link.take_output()
    }
    pub fn deadline(&self) -> Option<u64> {
        self.link
            .deadline()
            .into_iter()
            .chain(self.wireless.as_ref().and_then(Wireless::deadline))
            .min()
    }
    pub fn stage(&self) -> &'static str {
        match self.wireless.as_ref().map(|w| &w.stage) {
            None => "IAP_LINK_NEGOTIATING",
            Some(Stage::Identifying) => "ACCESSORY_IDENTIFYING",
            Some(Stage::Authenticating) => "ACCESSORY_AUTHENTICATING",
            Some(Stage::Ready) => "WIFI_HANDOFF_PENDING",
            Some(Stage::Failed) => "ACCESSORY_AUTH_FAILED",
        }
    }
    pub fn ready(&self) -> bool {
        self.wireless
            .as_ref()
            .is_some_and(|w| w.stage == Stage::Ready)
    }
    pub fn set_vehicle(&mut self,enabled:bool,values:crate::vehicle::Values,now:u64)->Result<()> {
        if !enabled && self.vehicle.is_none() {return Ok(());}
        let values=if enabled {values}else{crate::vehicle::Values::default()};
        let vehicle=self.vehicle.get_or_insert_with(Default::default);
        if let Some(message)=vehicle.set(values)? {self.link.send_control(&message.encode()?,now)?;}
        Ok(())
    }
    pub fn stop(&mut self) {
        self.link.eof();
        self.wireless = None;
        let _ = self.csm.eof();
    }
    pub fn advance(&mut self, now: u64) -> Result<()> {
        self.link.advance(now)?;
        if let Some(w) = self.wireless.as_mut() {
            w.advance(now)?
        }
        Ok(())
    }
    pub fn feed(
        &mut self,
        bytes: &[u8],
        endpoint: &Endpoint,
        identity: &dyn Identity,
        now: u64,
    ) -> Result<()> {
        let result = (|| {
            self.link.feed(bytes, now)?;
            while let Some(event) = self.link.event() {
                match event {
                    Event::Ready => self.wireless = Some(Wireless::new(now)),
                    Event::Dead(e) => return Err(e),
                    Event::Control(data) => {
                        let mut messages = Vec::new();
                        self.csm.feed(&data, |m| messages.push(m))?;
                        for message in messages {
                            if matches!(message.id,0xa100|0xa102) && self.ready() {
                                if let Some(vehicle)=&mut self.vehicle {if let Some(reply)=vehicle.handle(&message)? {self.link.send_control(&reply.encode()?,now)?;}}
                                continue;
                            }
                            let responses = self
                                .wireless
                                .as_mut()
                                .ok_or(Error::State)?
                                .handle(&message, endpoint, identity, now)?;
                            for mut response in responses {
                                if response.id==0x1d01 && self.vehicle.is_some() {response=crate::vehicle_status::VehicleStatus::identification(response)?;}
                                self.link.send_control(&response.encode()?, now)?;
                            }
                        }
                    }
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.stop()
        }
        result
    }
}

#[cfg(test)]
mod vehicle_tests {
    use super::*;
    use crate::{framing::Csm, vehicle::Values};
    #[test]
    fn disabled_snapshot_cannot_resubscribe_cached_vehicle_values() {
        let mut bootstrap=Bootstrap::new(true,0).unwrap();
        bootstrap.set_vehicle(false,Values::default(),0).unwrap();
        assert!(bootstrap.vehicle.is_none(),"unconfigured sessions do not declare EV status");
        bootstrap.set_vehicle(true,Values{soc_pct:Some(68.0),range_km:Some(320.0),..Default::default()},0).unwrap();
        let start=Csm{id:0xa100,body:vec![]};
        assert!(bootstrap.vehicle.as_mut().unwrap().handle(&start).unwrap().is_some());
        // watch may coalesce the transient stopped snapshot, leaving only disabled.
        bootstrap.set_vehicle(false,Values::default(),1).unwrap();
        assert!(bootstrap.vehicle.as_mut().unwrap().handle(&start).unwrap().is_none(),"disabled configuration must not actively resend old SOC/range");
        bootstrap.vehicle.as_mut().unwrap().handle(&Csm{id:0xa102,body:vec![]}).unwrap();
        bootstrap.set_vehicle(true,Values{soc_pct:Some(0.0),..Default::default()},2).unwrap();
        assert!(bootstrap.vehicle.as_mut().unwrap().handle(&start).unwrap().is_some(),"a new valid zero SOC can recover after reconfiguration");
    }
}
