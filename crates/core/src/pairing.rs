// Pair-verify responder ported from PlayPort / DiPlay (GPL-3.0-only).
// Imported accessory signing identity and previously trusted controller key are mandatory.
use crate::{
    crypto::{derive, open, seal},
    framing::{tlv8_decode, tlv8_encode},
    Error, Result,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, Zeroizing};
struct Pending {
    shared: [u8; 32],
    key: [u8; 32],
    own: [u8; 32],
    peer: [u8; 32],
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.shared.zeroize();
        self.key.zeroize();
    }
}
pub struct PairVerify {
    id: Vec<u8>,
    signing: SigningKey,
    pending: Option<Pending>,
}
pub struct Verified {
    pub shared: Zeroizing<[u8; 32]>,
    pub response: Vec<u8>,
    pub read_key: Zeroizing<[u8; 32]>,
    pub write_key: Zeroizing<[u8; 32]>,
    pub controller_id: Vec<u8>,
}
fn label(s: &[u8; 8]) -> [u8; 12] {
    let mut n = [0; 12];
    n[4..].copy_from_slice(s);
    n
}
impl PairVerify {
    pub fn new(id: Vec<u8>, imported_signing_key: [u8; 32]) -> Result<Self> {
        if id.is_empty() || id.len() > 128 {
            return Err(Error::Limit);
        }
        Ok(Self {
            id,
            signing: SigningKey::from_bytes(&imported_signing_key),
            pending: None,
        })
    }
    pub fn begin(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        self.pending = None;
        let msg = tlv8_decode(request)?;
        if msg.get(&6).map(Vec::as_slice) != Some(&[1]) {
            return Err(Error::State);
        }
        let peer: [u8; 32] = msg
            .get(&3)
            .ok_or(Error::Malformed)?
            .as_slice()
            .try_into()
            .map_err(|_| Error::Malformed)?;
        let ephemeral = StaticSecret::random_from_rng(rand_core::OsRng);
        let own = PublicKey::from(&ephemeral).to_bytes();
        let shared = ephemeral.diffie_hellman(&PublicKey::from(peer));
        if !shared.was_contributory() {
            return Err(Error::Authentication);
        }
        let key = derive(
            shared.as_bytes(),
            b"Pair-Verify-Encrypt-Salt",
            b"Pair-Verify-Encrypt-Info",
        )?;
        let mut transcript = own.to_vec();
        transcript.extend(&self.id);
        transcript.extend(peer);
        let sig = self.signing.sign(&transcript).to_bytes();
        let sub = tlv8_encode(&[(1, &self.id), (10, &sig)])?;
        let sealed = seal(&key, &label(b"PV-Msg02"), &sub, &[])?;
        self.pending = Some(Pending {
            shared: shared.to_bytes(),
            key,
            own,
            peer,
        });
        tlv8_encode(&[(6, &[2]), (3, &own), (5, &sealed)])
    }
    pub fn finish(
        &mut self,
        request: &[u8],
        trusted: impl FnOnce(&[u8]) -> Option<[u8; 32]>,
    ) -> Result<Verified> {
        let pending = self.pending.take().ok_or(Error::State)?; // single-use even on failure
        let msg = tlv8_decode(request)?;
        if msg.get(&6).map(Vec::as_slice) != Some(&[3]) {
            return Err(Error::State);
        }
        let plain = open(
            &pending.key,
            &label(b"PV-Msg03"),
            msg.get(&5).ok_or(Error::Malformed)?,
            &[],
        )?;
        let sub = tlv8_decode(&plain)?;
        let id = sub.get(&1).ok_or(Error::Malformed)?;
        if id.len() > 128 {
            return Err(Error::Limit);
        }
        let key = trusted(id).ok_or(Error::Authentication)?;
        let mut transcript = pending.peer.to_vec();
        transcript.extend(id);
        transcript.extend(pending.own);
        let signature = Signature::from_slice(sub.get(&10).ok_or(Error::Malformed)?)
            .map_err(|_| Error::Authentication)?;
        VerifyingKey::from_bytes(&key)
            .map_err(|_| Error::Authentication)?
            .verify_strict(&transcript, &signature)
            .map_err(|_| Error::Authentication)?;
        Ok(Verified {
            shared: Zeroizing::new(pending.shared),
            response: tlv8_encode(&[(6, &[4])])?,
            read_key: Zeroizing::new(derive(
                &pending.shared,
                b"Control-Salt",
                b"Control-Write-Encryption-Key",
            )?),
            write_key: Zeroizing::new(derive(
                &pending.shared,
                b"Control-Salt",
                b"Control-Read-Encryption-Key",
            )?),
            controller_id: id.clone(),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // Fixed synthetic signing seeds are never credentials, exported or installed.
    #[test]
    fn synthetic_controller_verifies_and_unknown_is_rejected() {
        for trust in [true, false] {
            let mut server = PairVerify::new(b"synthetic-accessory".to_vec(), [11; 32]).unwrap();
            let phone = StaticSecret::from([22; 32]);
            let phone_public = PublicKey::from(&phone).to_bytes();
            let req = tlv8_encode(&[(6, &[1]), (3, &phone_public)]).unwrap();
            let m2 = tlv8_decode(&server.begin(&req).unwrap()).unwrap();
            let own: [u8; 32] = m2[&3].as_slice().try_into().unwrap();
            let shared = phone.diffie_hellman(&PublicKey::from(own));
            let key = derive(
                shared.as_bytes(),
                b"Pair-Verify-Encrypt-Salt",
                b"Pair-Verify-Encrypt-Info",
            )
            .unwrap();
            let response =
                tlv8_decode(&open(&key, &label(b"PV-Msg02"), &m2[&5], &[]).unwrap()).unwrap();
            let mut transcript = own.to_vec();
            transcript.extend(&response[&1]);
            transcript.extend(phone_public);
            SigningKey::from_bytes(&[11; 32])
                .verifying_key()
                .verify_strict(&transcript, &Signature::from_slice(&response[&10]).unwrap())
                .unwrap();
            let controller = SigningKey::from_bytes(&[33; 32]);
            let mut transcript = phone_public.to_vec();
            transcript.extend(b"synthetic-phone");
            transcript.extend(own);
            let sig = controller.sign(&transcript).to_bytes();
            let sub = tlv8_encode(&[(1, b"synthetic-phone"), (10, &sig)]).unwrap();
            let cipher = seal(&key, &label(b"PV-Msg03"), &sub, &[]).unwrap();
            let m3 = tlv8_encode(&[(6, &[3]), (5, &cipher)]).unwrap();
            let result = server.finish(&m3, |id| {
                if trust && id == b"synthetic-phone" {
                    Some(controller.verifying_key().to_bytes())
                } else {
                    None
                }
            });
            assert_eq!(result.is_ok(), trust);
            assert!(server.finish(&m3, |_| None).is_err());
        }
    }
    #[test]
    fn low_order_peer_is_rejected() {
        let mut s = PairVerify::new(b"test".to_vec(), [11; 32]).unwrap();
        assert!(s
            .begin(&tlv8_encode(&[(6, &[1]), (3, &[0; 32])]).unwrap())
            .is_err());
    }
}

pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(seed).verifying_key().to_bytes()
}
