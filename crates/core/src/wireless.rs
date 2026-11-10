// Wireless identification / authenticated Wi-Fi handoff, PlayPort/DiPlay protocol, GPL-3.0-only.
use crate::{
    framing::{params, Csm},
    mfi::{parameter, AuthState, IapAuthentication, Identity},
    Error, Result,
};
use zeroize::Zeroizing;
pub struct Endpoint {
    pub name: String,
    pub model: String,
    pub manufacturer: String,
    pub serial: String,
    pub bluetooth_mac: [u8; 6],
    pub ssid: String,
    pub password: Zeroizing<String>,
    pub channel: u8,
    pub security: u8,
    pub addresses: Vec<std::net::IpAddr>,
    pub airplay_port: u16,
    pub pairing_public_key: [u8; 32],
    pub source_version: String,
}
fn text(s: &str) -> Result<Vec<u8>> {
    if s.len() > 256 || s.as_bytes().contains(&0) {
        return Err(Error::Malformed);
    }
    let mut b = s.as_bytes().to_vec();
    b.push(0);
    Ok(b)
}
fn body(parts: &[(u16, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for (id, b) in parts {
        out.extend(parameter(*id, b)?)
    }
    if out.len() > 65529 {
        return Err(Error::Limit);
    }
    Ok(out)
}
fn ids(values: &[u16]) -> Vec<u8> {
    values.iter().flat_map(|x| x.to_be_bytes()).collect()
}
impl Endpoint {
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty()
            || self.model.is_empty()
            || self.manufacturer.is_empty()
            || self.serial.is_empty()
            || !crate::validation::hotspot(&self.ssid, &self.password)
            || self.channel == 0
            || self.airplay_port == 0
            || self.addresses.is_empty()
            || self.addresses.len() > 4
            || self
                .addresses
                .iter()
                .any(|a| a.is_unspecified() || a.is_multicast())
        {
            return Err(Error::Malformed);
        }
        for s in [
            &self.name,
            &self.model,
            &self.manufacturer,
            &self.serial,
            &self.ssid,
            &self.password,
            &self.source_version,
        ] {
            text(s)?;
        }
        Ok(())
    }
    pub fn identification(&self) -> Result<Csm> {
        self.validate()?;
        let bt = body(&[
            (0, 0u16.to_be_bytes().to_vec()),
            (1, text("blue")?),
            (2, vec![]),
            (3, self.bluetooth_mac.to_vec()),
            (4, text("blue")?),
            (5, vec![]),
        ])?;
        let wifi = body(&[
            (0, 1u16.to_be_bytes().to_vec()),
            (1, text(&self.ssid)?),
            (2, vec![]),
            (3, 1u16.to_be_bytes().to_vec()),
            (4, vec![]),
            (5, vec![]),
        ])?;
        // No invented vehicle, location, USB, or external-accessory capabilities.
        let b = body(&[
            (0, text(&self.name)?),
            (1, text(&self.model)?),
            (2, text(&self.manufacturer)?),
            (3, text(&self.serial)?),
            (4, text("0.1")?),
            (5, text("1")?),
            (6, ids(&[0xaa01, 0xaa03, 0x5703, 0x4301])),
            (
                7,
                ids(&[
                    0xaa00, 0xaa02, 0xaa04, 0xaa05, 0x5702, 0x4300, 0x4e0d, 0x4e0e,
                ]),
            ),
            (8, vec![0]),
            (9, 20u16.to_be_bytes().to_vec()),
            (12, text("en")?),
            (13, text("en")?),
            (17, bt),
            (24, wifi),
        ])?;
        Ok(Csm {
            id: 0x1d01,
            body: b,
        })
    }
    fn wifi(&self) -> Result<Csm> {
        Ok(Csm {
            id: 0x5703,
            body: body(&[
                (1, text(&self.ssid)?),
                (2, text(&self.password)?),
                (3, vec![self.security]),
                (4, vec![self.channel]),
            ])?,
        })
    }
    fn start_session(&self) -> Result<Csm> {
        let mut wifi = vec![
            (0, text(&self.ssid)?),
            (1, text(&self.password)?),
            (2, vec![self.channel]),
        ];
        for ip in &self.addresses {
            wifi.push((3, text(&ip.to_string())?))
        }
        wifi.push((4, vec![self.security]));
        let public = self
            .pairing_public_key
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        Ok(Csm {
            id: 0x4301,
            body: body(&[
                (1, body(&wifi)?),
                (2, (self.airplay_port as u32).to_be_bytes().to_vec()),
                (4, text(&public)?),
                (5, text(&self.source_version)?),
            ])?,
        })
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Stage {
    Identifying,
    Authenticating,
    Ready,
    Failed,
}
pub struct Wireless {
    pub stage: Stage,
    deadline: u64,
    identified_sent: bool,
    auth: Option<IapAuthentication>,
    before: u8,
    after: u8,
    transport: bool,
    invites: u8,
}
impl Wireless {
    pub fn new(now: u64) -> Self {
        Self {
            stage: Stage::Identifying,
            deadline: now.saturating_add(10000),
            identified_sent: false,
            auth: None,
            before: 0,
            after: 0,
            transport: false,
            invites: 0,
        }
    }
    pub fn deadline(&self) -> Option<u64> {
        if matches!(self.stage, Stage::Identifying | Stage::Authenticating) {
            Some(self.deadline)
        } else {
            None
        }
    }
    pub fn advance(&mut self, now: u64) -> Result<()> {
        if matches!(self.stage, Stage::Identifying | Stage::Authenticating) && now >= self.deadline
        {
            self.stage = Stage::Failed;
            return Err(Error::Exhausted);
        }
        Ok(())
    }
    pub fn handle(
        &mut self,
        f: &Csm,
        endpoint: &Endpoint,
        identity: &dyn Identity,
        now: u64,
    ) -> Result<Vec<Csm>> {
        let result = (|| {
            self.advance(now)?;
            match self.stage {
                Stage::Identifying => match f.id {
                    0x1d00 => {
                        if self.identified_sent {
                            return Err(Error::State);
                        }
                        self.identified_sent = true;
                        Ok(vec![endpoint.identification()?])
                    }
                    0x1d02 if self.identified_sent => {
                        self.stage = Stage::Authenticating;
                        self.auth = Some(IapAuthentication::new(now));
                        self.deadline = now.saturating_add(30000);
                        Ok(vec![])
                    }
                    0x1d03 => Err(Error::Authentication),
                    _ => Err(Error::State),
                },
                Stage::Authenticating => {
                    let auth = self.auth.as_mut().ok_or(Error::State)?;
                    let response = auth.handle(f, identity, now)?;
                    if auth.state == AuthState::Accepted {
                        self.stage = Stage::Ready;
                    }
                    Ok(response.into_iter().collect())
                }
                Stage::Ready => match f.id {
                    0x5702 | 0x4e0e => {
                        if f.id == 0x4e0e {
                            params(&f.body)?;
                            self.transport = true;
                        }
                        let count = if self.transport {
                            &mut self.after
                        } else {
                            &mut self.before
                        };
                        let max = if self.transport { 2 } else { 5 };
                        if *count >= max {
                            return Ok(vec![]);
                        }
                        *count += 1;
                        Ok(vec![endpoint.wifi()?])
                    }
                    0x4300 => {
                        params(&f.body)?;
                        if self.invites >= 3 {
                            return Ok(vec![]);
                        }
                        self.invites += 1;
                        Ok(vec![endpoint.start_session()?])
                    }
                    0x4e0d => {
                        let p = params(&f.body)?;
                        if !p.iter().any(|(id, b)| *id == 0 && (*b == [0] || *b == [1])) {
                            return Err(Error::Malformed);
                        }
                        Ok(vec![])
                    }
                    _ => Ok(vec![]),
                },
                Stage::Failed => Err(Error::State),
            }
        })();
        if result.is_err() {
            self.stage = Stage::Failed;
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Synthetic;
    impl Identity for Synthetic {
        fn certificate(&self) -> &[u8] {
            b"SYNTHETIC"
        }
        fn sign_digest(&self, _: &[u8; 32]) -> Result<[u8; 64]> {
            Ok([0; 64])
        }
    } // message-state test only; crypto is tested in mfi.
    fn endpoint() -> Endpoint {
        Endpoint {
            name: "test".into(),
            model: "test".into(),
            manufacturer: "test".into(),
            serial: "synthetic".into(),
            bluetooth_mac: [2; 6],
            ssid: "synthetic".into(),
            password: Zeroizing::new("not-real-password".into()),
            channel: 36,
            security: 2,
            addresses: vec!["192.0.2.1".parse().unwrap()],
            airplay_port: 7000,
            pairing_public_key: [3; 32],
            source_version: "366.0".into(),
        }
    }
    #[test]
    fn no_wifi_secrets_before_authentication() {
        let mut w = Wireless::new(0);
        assert!(w
            .handle(
                &Csm {
                    id: 0x5702,
                    body: vec![]
                },
                &endpoint(),
                &Synthetic,
                1
            )
            .is_err());
        assert_eq!(w.stage, Stage::Failed);
    }
    #[test]
    fn identification_authentication_wifi_and_limited_resends() {
        let mut w = Wireless::new(0);
        let e = endpoint();
        for (id, b) in [
            (0x1d00, vec![]),
            (0x1d02, vec![]),
            (0xaa00, vec![]),
            (0xaa02, parameter(0, &[0; 32]).unwrap()),
            (0xaa05, vec![]),
        ] {
            w.handle(&Csm { id, body: b }, &e, &Synthetic, 1).unwrap();
        }
        assert_eq!(w.stage, Stage::Ready);
        for _ in 0..5 {
            assert_eq!(
                w.handle(
                    &Csm {
                        id: 0x5702,
                        body: vec![]
                    },
                    &e,
                    &Synthetic,
                    2
                )
                .unwrap()[0]
                    .id,
                0x5703
            )
        }
        assert!(w
            .handle(
                &Csm {
                    id: 0x5702,
                    body: vec![]
                },
                &e,
                &Synthetic,
                2
            )
            .unwrap()
            .is_empty());
        let start = w
            .handle(
                &Csm {
                    id: 0x4300,
                    body: vec![],
                },
                &e,
                &Synthetic,
                2,
            )
            .unwrap();
        assert_eq!(start[0].id, 0x4301);
        assert!(params(&start[0].body)
            .unwrap()
            .iter()
            .any(|(id, b)| *id == 2 && *b == 7000u32.to_be_bytes()));
    }
}
