use super::{
    climate, decode_state,
    proto::{self, Message},
    Category, ClimateCommand, ClimateState, Error, Result, Update,
};
use p256::{
    elliptic_curve::sec1::ToEncodedPoint,
    pkcs8::{DecodePrivateKey, EncodePrivateKey},
    PublicKey, SecretKey,
};
use rand_core::{OsRng, RngCore};
use ring::{aead, hmac};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub struct Identity(SecretKey);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentPermission {
    VehicleMonitor,
    Driver,
}
impl Identity {
    pub fn generate() -> Self {
        Self(SecretKey::random(&mut OsRng))
    }
    pub fn from_pkcs8(bytes: &[u8]) -> Result<Self> {
        SecretKey::from_pkcs8_der(bytes)
            .map(Self)
            .map_err(|_| Error::Authentication)
    }
    pub fn pkcs8(&self) -> Result<Zeroizing<Vec<u8>>> {
        self.0
            .to_pkcs8_der()
            .map(|d| Zeroizing::new(d.as_bytes().to_vec()))
            .map_err(|_| Error::Authentication)
    }
    pub fn public_key(&self) -> Vec<u8> {
        self.0
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec()
    }
    /// Enrollment request with Tesla's Vehicle Monitor role (read access, no owner/driver grant).
    /// This request still requires the vehicle owner's physical NFC/UI confirmation.
    pub fn enrollment_request(&self) -> Vec<u8> {
        self.enrollment_request_for(EnrollmentPermission::VehicleMonitor)
    }
    pub fn enrollment_request_for(&self, permission: EnrollmentPermission) -> Vec<u8> {
        let role = match permission {
            EnrollmentPermission::VehicleMonitor => 5,
            EnrollmentPermission::Driver => 3,
        };
        let mut permission = proto::nested(1, &proto::nested(1, &self.public_key()));
        proto::int(4, role, &mut permission);
        let mut whitelist = proto::nested(5, &permission);
        proto::bytes(6, &[], &mut whitelist);
        let unsigned = proto::nested(16, &whitelist);
        let mut signed = proto::nested(2, &unsigned);
        proto::int(3, 2, &mut signed);
        proto::nested(1, &signed)
    }
    fn shared(&self, remote: &[u8]) -> Result<Zeroizing<[u8; 16]>> {
        let remote = PublicKey::from_sec1_bytes(remote).map_err(|_| Error::Authentication)?;
        let shared = p256::ecdh::diffie_hellman(self.0.to_nonzero_scalar(), remote.as_affine());
        let digest = sha1::Sha1::digest(shared.raw_secret_bytes());
        Ok(Zeroizing::new(digest[..16].try_into().unwrap()))
    }
}
fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    OsRng.fill_bytes(&mut bytes);
    bytes
}
fn metadata(parts: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (tag, value) in parts {
        out.extend([*tag, value.len() as u8]);
        out.extend(*value);
    }
    out.push(255);
    out
}
fn cipher(key: &[u8]) -> Result<aead::LessSafeKey> {
    aead::UnboundKey::new(&aead::AES_128_GCM, key)
        .map(aead::LessSafeKey::new)
        .map_err(|_| Error::Authentication)
}
fn seal(key: &[u8], nonce: [u8; 12], aad: &[u8], plain: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut bytes = plain.to_vec();
    let tag = cipher(key)?
        .seal_in_place_separate_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(aad),
            &mut bytes,
        )
        .map_err(|_| Error::Authentication)?;
    Ok((bytes, tag.as_ref().to_vec()))
}
fn command_aad(vin: &[u8], epoch: &[u8], expires: u32, counter: u32) -> [u8; 32] {
    Sha256::digest(metadata(&[
        (0, &[5]),
        (1, &[3]),
        (2, vin),
        (3, epoch),
        (4, &expires.to_be_bytes()),
        (5, &counter.to_be_bytes()),
        (7, &2u32.to_be_bytes()),
    ]))
    .into()
}
fn session_tag(key: &[u8], vin: &[u8], challenge: &[u8], raw: &[u8]) -> Vec<u8> {
    let sub = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), b"session info");
    let mut data = metadata(&[(0, &[6]), (2, vin), (6, challenge)]);
    data.extend(raw);
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, sub.as_ref()), &data)
        .as_ref()
        .to_vec()
}
struct Session {
    key: Zeroizing<[u8; 16]>,
    remote_public: Vec<u8>,
    epoch: [u8; 16],
    counter: u32,
    clock: u32,
    since: Instant,
}
struct Pending {
    uuid: [u8; 16],
    since: Instant,
    request: Option<Request>,
    request_id: Vec<u8>,
    last_response: Option<u32>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Request {
    Read(Category),
    ClimateQuery,
    ClimateCommand,
}
pub struct Client {
    vin: String,
    identity: Identity,
    route: [u8; 16],
    pending: Option<Pending>,
    session: Option<Session>,
}
impl Client {
    pub fn new(vin: &str, identity: Identity) -> Result<Self> {
        if vin.len() != 17
            || !vin
                .bytes()
                .all(|c| c.is_ascii_digit() || (c.is_ascii_uppercase() && !b"IOQ".contains(&c)))
        {
            return Err(Error::Malformed);
        }
        Ok(Self {
            vin: vin.into(),
            identity,
            route: random(),
            pending: None,
            session: None,
        })
    }
    pub fn local_name(&self) -> String {
        let digest = sha1::Sha1::digest(self.vin.as_bytes());
        format!(
            "S{}C",
            digest[..8]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    }
    fn envelope(&self, uuid: &[u8; 16]) -> Vec<u8> {
        let mut m = proto::nested(6, &[8, 3]);
        proto::bytes(7, &proto::nested(2, &self.route), &mut m);
        proto::bytes(51, uuid, &mut m);
        m
    }
    pub fn hello(&mut self) -> Vec<u8> {
        self.session = None;
        let uuid = random();
        let mut request = Vec::new();
        proto::bytes(1, &self.identity.public_key(), &mut request);
        let mut message = self.envelope(&uuid);
        proto::bytes(14, &request, &mut message);
        self.pending = Some(Pending {
            uuid,
            since: Instant::now(),
            request: None,
            request_id: Vec::new(),
            last_response: None,
        });
        message
    }
    fn matches(&self, m: &Message<'_>) -> Result<bool> {
        let Some(p) = &self.pending else {
            return Ok(false);
        };
        let from = m
            .nested(7)?
            .and_then(|d| d.int(1).transpose())
            .transpose()?;
        let to = m
            .nested(6)?
            .and_then(|d| d.bytes(2).transpose())
            .transpose()?;
        Ok(from == Some(3)
            && to == Some(self.route.as_slice())
            && m.bytes(50)? == Some(p.uuid.as_slice()))
    }
    pub fn accept_hello(&mut self, wire: &[u8]) -> Result<bool> {
        let m = Message::parse(wire)?;
        if !self.matches(&m)? {
            return Ok(false);
        }
        let p = self.pending.as_ref().ok_or(Error::State)?;
        if p.request.is_some() {
            return Err(Error::State);
        }
        if p.since.elapsed() > Duration::from_secs(4) {
            self.pending = None;
            return Err(Error::Expired);
        }
        let fault = m
            .nested(12)?
            .and_then(|s| s.int(2).transpose())
            .transpose()?
            .unwrap_or(0);
        let Some(raw) = m.bytes(15)? else {
            return Err(if matches!(fault, 3 | 4 | 7) {
                Error::NotAuthorized
            } else {
                Error::VehicleFault(u32::try_from(fault).map_err(|_| Error::Malformed)?)
            });
        };
        let info = Message::parse(raw)?;
        let key = self
            .identity
            .shared(info.bytes(2)?.ok_or(Error::Malformed)?)?;
        let tag = m
            .nested(13)?
            .and_then(|s| s.nested(6).transpose())
            .transpose()?
            .ok_or(Error::Authentication)?;
        let expected = session_tag(&key[..], self.vin.as_bytes(), &p.uuid, raw);
        if !bool::from(subtle::ConstantTimeEq::ct_eq(
            expected.as_slice(),
            tag.bytes(1)?.ok_or(Error::Authentication)?,
        )) {
            return Err(Error::Authentication);
        }
        if info.int(5)?.unwrap_or(0) == 1 {
            return Err(Error::NotAuthorized);
        }
        if info.int(5)?.unwrap_or(0) != 0 {
            return Err(Error::Authentication);
        }
        if fault != 0 {
            return Err(Error::VehicleFault(
                u32::try_from(fault).map_err(|_| Error::Malformed)?,
            ));
        }
        let epoch = info
            .bytes(3)?
            .ok_or(Error::Malformed)?
            .try_into()
            .map_err(|_| Error::Malformed)?;
        let counter = u32::try_from(info.int(1)?.unwrap_or(0)).map_err(|_| Error::Malformed)?;
        let clock = info.fixed(4)?.ok_or(Error::Malformed)?;
        self.session = Some(Session {
            key,
            remote_public: info.bytes(2)?.ok_or(Error::Malformed)?.to_vec(),
            epoch,
            counter,
            clock,
            since: Instant::now(),
        });
        self.pending = None;
        Ok(true)
    }
    /// Retire a failed/cancelled query without reusing its UUID or resetting the session counter.
    pub fn cancel_query(&mut self) {
        if self.pending.as_ref().is_some_and(|p| p.request.is_some()) {
            self.pending = None;
        }
    }
    pub fn query(&mut self, category: Category) -> Result<Vec<u8>> {
        self.request(Request::Read(category), &category.payload())
    }
    pub fn climate_query(&mut self) -> Result<Vec<u8>> {
        self.request(Request::ClimateQuery, &climate::query())
    }
    pub fn climate_command(&mut self, command: ClimateCommand) -> Result<Vec<u8>> {
        self.request(Request::ClimateCommand, &command.payload()?)
    }
    fn request(&mut self, request: Request, payload: &[u8]) -> Result<Vec<u8>> {
        if self.pending.is_some() {
            return Err(Error::State);
        }
        let s = self.session.as_mut().ok_or(Error::State)?;
        s.counter = s.counter.checked_add(1).ok_or(Error::Exhausted)?;
        let elapsed = u32::try_from(s.since.elapsed().as_secs()).map_err(|_| Error::Expired)?;
        let expires = s
            .clock
            .checked_add(elapsed)
            .and_then(|n| n.checked_add(5))
            .ok_or(Error::Expired)?;
        let nonce = random();
        let aad = command_aad(self.vin.as_bytes(), &s.epoch, expires, s.counter);
        let (encrypted, tag) = seal(&s.key[..], nonce, &aad, payload)?;
        let mut auth = Vec::new();
        proto::bytes(1, &s.epoch, &mut auth);
        proto::bytes(2, &nonce, &mut auth);
        proto::int(3, s.counter.into(), &mut auth);
        proto::fixed(4, expires, &mut auth);
        proto::bytes(5, &tag, &mut auth);
        let mut signature = proto::nested(1, &proto::nested(1, &self.identity.public_key()));
        proto::bytes(5, &auth, &mut signature);
        let uuid = random();
        let mut message = self.envelope(&uuid);
        proto::bytes(10, &encrypted, &mut message);
        proto::bytes(13, &signature, &mut message);
        proto::int(52, 2, &mut message);
        let mut request_id = vec![5];
        request_id.extend(&tag);
        self.pending = Some(Pending {
            uuid,
            since: Instant::now(),
            request: Some(request),
            request_id,
            last_response: None,
        });
        Ok(message)
    }
    pub fn accept_response(&mut self, wire: &[u8], received_ms: u64) -> Result<Option<Update>> {
        let category = self.pending.as_ref().and_then(|p| match p.request {
            Some(Request::Read(category)) => Some(category),
            _ => None,
        });
        self.accept_payload(wire, category.map(Request::Read))?
            .map(|bytes| decode_state(category.ok_or(Error::State)?, &bytes, received_ms))
            .transpose()
    }
    pub fn accept_climate_response(&mut self, wire: &[u8]) -> Result<Option<ClimateState>> {
        self.accept_payload(wire, Some(Request::ClimateQuery))?
            .map(|bytes| climate::decode(&bytes))
            .transpose()
    }
    pub fn accept_climate_ack(&mut self, wire: &[u8]) -> Result<bool> {
        let Some(bytes) = self.accept_payload(wire, Some(Request::ClimateCommand))? else {
            return Ok(false);
        };
        climate::ack(&bytes)?;
        Ok(true)
    }
    fn accept_payload(
        &mut self,
        wire: &[u8],
        expected: Option<Request>,
    ) -> Result<Option<Vec<u8>>> {
        let m = Message::parse(wire)?;
        if !self.matches(&m)? {
            return Ok(None);
        }
        let p = self.pending.as_ref().ok_or(Error::State)?;
        if p.request.is_none() || p.request != expected {
            return Err(Error::State);
        }
        if p.since.elapsed() > Duration::from_secs(5) {
            self.pending = None;
            return Err(Error::Expired);
        }
        let status = m.nested(12)?;
        let fault = status
            .as_ref()
            .map(|s| s.int(2))
            .transpose()?
            .flatten()
            .unwrap_or(0);
        if let Some(raw) = m.bytes(15)? {
            if p.since.elapsed() > Duration::from_secs(4) {
                return Err(Error::Expired);
            }
            let tag = m
                .nested(13)?
                .and_then(|s| s.nested(6).transpose())
                .transpose()?
                .ok_or(Error::Authentication)?;
            let s = self.session.as_mut().ok_or(Error::State)?;
            let expected = session_tag(&s.key[..], self.vin.as_bytes(), &p.uuid, raw);
            if !bool::from(subtle::ConstantTimeEq::ct_eq(
                expected.as_slice(),
                tag.bytes(1)?.ok_or(Error::Authentication)?,
            )) {
                return Err(Error::Authentication);
            }
            let info = Message::parse(raw)?;
            if info.bytes(2)? != Some(s.remote_public.as_slice()) {
                return Err(Error::Authentication);
            }
            if info.int(5)?.unwrap_or(0) != 0 {
                return Err(Error::NotAuthorized);
            }
            let epoch: [u8; 16] = info
                .bytes(3)?
                .ok_or(Error::Malformed)?
                .try_into()
                .map_err(|_| Error::Malformed)?;
            let clock = info.fixed(4)?.ok_or(Error::Malformed)?;
            if epoch == s.epoch && clock < s.clock {
                return Err(Error::Authentication);
            }
            s.counter = s
                .counter
                .max(u32::try_from(info.int(1)?.unwrap_or(0)).map_err(|_| Error::Malformed)?);
            s.epoch = epoch;
            s.clock = clock;
            s.since = Instant::now();
            self.pending = None;
            return Err(Error::Resync);
        }
        let operation = status
            .as_ref()
            .map(|s| s.int(1))
            .transpose()?
            .flatten()
            .unwrap_or(0);
        let encrypted = m
            .nested(13)?
            .and_then(|s| s.nested(9).transpose())
            .transpose()?;
        if encrypted.is_none() && matches!(fault, 3 | 4 | 7) {
            return Err(Error::NotAuthorized);
        }
        if encrypted.is_none() && operation == 1 && fault == 0 {
            return Ok(None);
        }
        if encrypted.is_none() && fault != 0 {
            return Err(Error::VehicleFault(
                u32::try_from(fault).map_err(|_| Error::Malformed)?,
            ));
        }
        let response = encrypted.ok_or(Error::Authentication)?;
        let counter = u32::try_from(response.int(2)?.unwrap_or(0)).map_err(|_| Error::Malformed)?;
        let flags = u32::try_from(m.int(52)?.unwrap_or(0)).map_err(|_| Error::Malformed)?;
        let fault = u32::try_from(fault).map_err(|_| Error::Malformed)?;
        let aad = Sha256::digest(metadata(&[
            (0, &[9]),
            (1, &[3]),
            (2, self.vin.as_bytes()),
            (5, &counter.to_be_bytes()),
            (7, &flags.to_be_bytes()),
            (8, &p.request_id),
            (9, &fault.to_be_bytes()),
        ]));
        let nonce: [u8; 12] = response
            .bytes(1)?
            .ok_or(Error::Authentication)?
            .try_into()
            .map_err(|_| Error::Authentication)?;
        let tag = response.bytes(3)?.ok_or(Error::Authentication)?;
        if tag.len() != 16 {
            return Err(Error::Authentication);
        }
        let mut plain = m.bytes(10)?.ok_or(Error::Malformed)?.to_vec();
        plain.extend(tag);
        let s = self.session.as_ref().ok_or(Error::State)?;
        let bytes = cipher(&s.key[..])?
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad.as_slice()),
                &mut plain,
            )
            .map_err(|_| Error::Authentication)?;
        if p.last_response.is_some_and(|previous| counter <= previous) {
            return Err(Error::Authentication);
        }
        if operation == 1 {
            self.pending.as_mut().unwrap().last_response = Some(counter);
            return Ok(None);
        }
        // One authenticated result consumes the request. Its replay or late replies cannot be reused.
        self.pending = None;
        if matches!(fault, 3 | 4 | 7) {
            return Err(Error::NotAuthorized);
        }
        if fault != 0 {
            return Err(Error::VehicleFault(fault));
        }
        if operation != 0 {
            return Err(Error::Rejected);
        }
        Ok(Some(bytes.to_vec()))
    }
}

#[cfg(test)]
pub(super) mod tests;
