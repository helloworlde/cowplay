//! iAP2 EV status, following the locked LIVI VehicleStatus declarations (see NOTICE.md).
use crate::{
    Error, Result,
    framing::{Csm, params},
    mfi::parameter,
    vehicle::Values,
};
#[derive(Default)]
pub struct VehicleStatus {
    wanted: bool,
    values: Values,
}
impl VehicleStatus {
    pub fn handle(&mut self, message: &Csm) -> Result<Option<Csm>> {
        params(&message.body)?;
        match message.id {
            0xa100 => {
                self.wanted = true;
                self.update()
            }
            0xa102 => {
                self.wanted = false;
                Ok(None)
            }
            _ => Err(Error::State),
        }
    }
    pub fn set(&mut self, values: Values) -> Result<Option<Csm>> {
        if self.values == values {
            return Ok(None);
        }
        self.values = values;
        self.update()
    }
    fn update(&self) -> Result<Option<Csm>> {
        if !self.wanted {
            return Ok(None);
        }
        let mut body = Vec::new();
        if let Some(range) = self
            .values
            .range_km
            .filter(|n| n.is_finite() && *n >= 0.0 && *n <= u16::MAX as f64)
        {
            let bytes = (range.round() as u16).to_be_bytes();
            body.extend(parameter(3, &bytes)?);
            body.extend(parameter(11, &bytes)?);
        }
        if let Some(soc) = self
            .values
            .soc_pct
            .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
        {
            body.extend(parameter(
                24,
                &((soc * 1000.0).round() as u32).to_be_bytes(),
            )?);
        }
        // Optional omission is not a documented native reset. Never invent 0/UINTMAX for absence.
        // The shared feed and Ultra availability state explicitly expose stale/disconnected data.
        Ok((!body.is_empty()).then_some(Csm { id: 0xa101, body }))
    }
    pub fn identification(mut message: Csm) -> Result<Csm> {
        let fields = params(&message.body)?;
        let name = fields
            .iter()
            .find(|(id, _)| *id == 0)
            .map(|(_, v)| *v)
            .ok_or(Error::Malformed)?;
        let mut body = Vec::new();
        for (id, value) in &fields {
            let mut v = value.to_vec();
            if *id == 6 {
                v.extend(0xa101u16.to_be_bytes());
            }
            if *id == 7 {
                v.extend(0xa100u16.to_be_bytes());
                v.extend(0xa102u16.to_be_bytes());
            }
            body.extend(parameter(*id, &v)?);
        }
        let mut information = parameter(0, &0u16.to_be_bytes())?;
        information.extend(parameter(1, name)?);
        information.extend(parameter(2, &[2])?);
        body.extend(parameter(20, &information)?);
        let mut status = parameter(0, &0u16.to_be_bytes())?;
        status.extend(parameter(1, name)?);
        for capability in [3, 11, 21] {
            status.extend(parameter(capability, &[])?);
        }
        body.extend(parameter(21, &status)?);
        message.body = body;
        Ok(message)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subscribed_ev_status_encodes_real_soc_and_km_only() {
        let mut v = VehicleStatus::default();
        assert!(
            v.set(Values {
                soc_pct: Some(68.123),
                range_km: Some(320.4),
                ..Default::default()
            })
            .unwrap()
            .is_none()
        );
        let message = v
            .handle(&Csm {
                id: 0xa100,
                body: vec![],
            })
            .unwrap()
            .unwrap();
        assert_eq!(message.id, 0xa101);
        let p = params(&message.body).unwrap();
        assert_eq!(
            p,
            vec![
                (3, &320u16.to_be_bytes()[..]),
                (11, &320u16.to_be_bytes()[..]),
                (24, &68123u32.to_be_bytes()[..])
            ]
        );
        assert!(
            v.handle(&Csm {
                id: 0xa102,
                body: vec![]
            })
            .unwrap()
            .is_none()
        );
        assert!(v.set(Values::default()).unwrap().is_none());
        assert!(
            v.handle(&Csm {
                id: 0xa100,
                body: vec![]
            })
            .unwrap()
            .is_none(),
            "absence never becomes fabricated zero SOC"
        );
    }
}
