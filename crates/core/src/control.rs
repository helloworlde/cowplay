//! Actual pairing/auth routing and encrypted-channel transition; no platform I/O.
use crate::{
    framing::tlv8_decode,
    mfi::{auth_setup, Identity},
    pairing::{PairVerify, Verified},
    rtsp::{response, Message},
    setup::{PairSetup, Paired},
    Error, Result,
};
pub enum Decision {
    Reply {
        wire: Vec<u8>,
        enable: Option<Verified>,
        paired: Option<Paired>,
    },
    Authenticated(Message),
}
pub struct Control {
    setup: PairSetup,
    verify: PairVerify,
    verified: bool,
    accessory_proof_sent: bool,
    requests: u8,
    pairing_deadline: u64,
    info: Vec<u8>,
}
impl Control {
    pub fn new(
        id: Vec<u8>,
        imported_signing_seed: [u8; 32],
        info: Vec<u8>,
        pairing_deadline: u64,
    ) -> Result<Self> {
        if info.len() > 262144 {
            return Err(Error::Limit);
        }
        Ok(Self {
            setup: PairSetup::new(id.clone(), imported_signing_seed)?,
            verify: PairVerify::new(id, imported_signing_seed)?,
            verified: false,
            accessory_proof_sent: false,
            requests: 0,
            pairing_deadline,
            info,
        })
    }
    pub fn handle(
        &mut self,
        request: Message,
        identity: &dyn Identity,
        lookup: impl FnOnce(&[u8]) -> Option<[u8; 32]>,
        now: u64,
    ) -> Result<Decision> {
        let mut enable = None;
        let mut paired = None;
        let path = request.path.as_str();
        if !self.verified {
            self.requests = self.requests.checked_add(1).ok_or(Error::Limit)?;
            if self.requests > 16 {
                return Err(Error::Limit);
            }
        }
        let (status, content, body) = match path {
            "/pair-setup" if !self.verified && request.method == "POST" => {
                let tlv = tlv8_decode(&request.body)?;
                let stage = tlv
                    .get(&6)
                    .and_then(|b| b.first())
                    .copied()
                    .ok_or(Error::Malformed)?;
                let body = match stage {
                    1 => self
                        .setup
                        .begin(&request.body, now, now < self.pairing_deadline)?,
                    3 => self.setup.proof(&request.body, now)?,
                    5 => {
                        let p = self.setup.finish(&request.body, now)?;
                        let b = p.response.clone();
                        paired = Some(p);
                        b
                    }
                    _ => return Err(Error::State),
                };
                (200, "application/pairing+tlv8", body)
            }
            "/pair-verify" if !self.verified && request.method == "POST" => {
                let tlv = tlv8_decode(&request.body)?;
                let stage = tlv
                    .get(&6)
                    .and_then(|b| b.first())
                    .copied()
                    .ok_or(Error::Malformed)?;
                let body = match stage {
                    1 => self.verify.begin(&request.body)?,
                    3 => {
                        let v = self.verify.finish(&request.body, lookup)?;
                        let b = v.response.clone();
                        enable = Some(v);
                        self.verified = true;
                        b
                    }
                    _ => return Err(Error::State),
                };
                (200, "application/pairing+tlv8", body)
            }
            "/auth-setup" if self.verified && request.method == "POST" => {
                let body = auth_setup(&request.body, identity)?;
                self.accessory_proof_sent = true;
                (200, "application/octet-stream", body)
            }
            "/info" => (200, "application/x-apple-binary-plist", self.info.clone()),
            _ if request.method == "OPTIONS" => (200, "application/octet-stream", vec![]),
            _ if self.verified && self.accessory_proof_sent => {
                return Ok(Decision::Authenticated(request))
            }
            _ => (401, "application/octet-stream", vec![]),
        };
        Ok(Decision::Reply {
            wire: response(&request, status, content, &body)?,
            enable,
            paired,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct NoIdentity;
    impl Identity for NoIdentity {
        fn certificate(&self) -> &[u8] {
            &[]
        }
        fn sign_digest(&self, _: &[u8; 32]) -> Result<[u8; 64]> {
            panic!("unauthenticated request must not reach identity")
        }
    }
    #[test]
    fn setup_record_and_mfi_blocked_until_pair_verification() {
        for method in ["SETUP", "RECORD", "POST"] {
            let mut c = Control::new(b"synthetic".to_vec(), [11; 32], vec![], 0).unwrap();
            let request = Message {
                method: method.into(),
                path: if method == "POST" {
                    "/auth-setup"
                } else {
                    "/stream"
                }
                .into(),
                protocol: "RTSP/1.0".into(),
                headers: Default::default(),
                body: vec![],
            };
            match c.handle(request, &NoIdentity, |_| None, 0).unwrap() {
                Decision::Reply { wire, .. } => assert!(wire.starts_with(b"RTSP/1.0 401")),
                _ => panic!(),
            }
        }
    }
}
