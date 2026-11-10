// AirPlay Pair-Setup variant; protocol source PlayPort/DiPlay, GPL-3.0-only.
// SRP uses BigUint modular exponentiation as upstream. It is NOT constant-time;
// this component requires side-channel review before production exposure.
use crate::{
    crypto::{derive, open, seal},
    framing::{tlv8_decode, tlv8_encode},
    Error, Result,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use num_bigint::BigUint as B;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha512};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;
const N_HEX:&str="FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E208E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF";
fn n() -> B {
    B::parse_bytes(N_HEX.as_bytes(), 16).unwrap()
}
fn hash(parts: &[&[u8]]) -> [u8; 64] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p)
    }
    h.finalize().into()
}
fn pad(v: &B) -> Result<[u8; 384]> {
    let b = v.to_bytes_be();
    if b.len() > 384 {
        return Err(Error::Limit);
    }
    let mut out = [0; 384];
    out[384 - b.len()..].copy_from_slice(&b);
    Ok(out)
}
fn label(s: &[u8; 8]) -> [u8; 12] {
    let mut n = [0; 12];
    n[4..].copy_from_slice(s);
    n
}
struct Srp {
    salt: [u8; 16],
    secret: Zeroizing<[u8; 32]>,
    verifier: B,
    public: B,
}
impl Srp {
    fn start() -> Self {
        let mut salt = [0; 16];
        OsRng.fill_bytes(&mut salt);
        let mut secret = Zeroizing::new([0; 32]);
        OsRng.fill_bytes(secret.as_mut());
        Self::with_random(salt, secret)
    }
    fn with_random(salt: [u8; 16], secret: Zeroizing<[u8; 32]>) -> Self {
        let n = n();
        let g = B::from(5u8);
        let x = B::from_bytes_be(&hash(&[&salt, &hash(&[b"Pair-Setup:3939"])]));
        let verifier = g.modpow(&x, &n);
        let k = B::from_bytes_be(&hash(&[&n.to_bytes_be(), &pad(&g).unwrap()]));
        let public = (&k * &verifier + g.modpow(&B::from_bytes_be(&*secret), &n)) % &n;
        Self {
            salt,
            secret,
            verifier,
            public,
        }
    }
    fn verify(self, a: &[u8], proof: &[u8]) -> Result<(Zeroizing<[u8; 64]>, [u8; 64])> {
        if a.len() != 384 || proof.len() != 64 {
            return Err(Error::Malformed);
        }
        let n = n();
        let a = B::from_bytes_be(a);
        if &a % &n == B::default() {
            return Err(Error::Authentication);
        }
        let ap = pad(&a)?;
        let bp = pad(&self.public)?;
        let u = B::from_bytes_be(&hash(&[&ap, &bp]));
        if u == B::default() {
            return Err(Error::Authentication);
        }
        let s =
            ((&a * self.verifier.modpow(&u, &n)) % &n).modpow(&B::from_bytes_be(&*self.secret), &n);
        let key = Zeroizing::new(hash(&[&s.to_bytes_be()]));
        let mut xor = hash(&[&n.to_bytes_be()]);
        for (x, g) in xor.iter_mut().zip(hash(&[&[5]])) {
            *x ^= g
        }
        let expected = hash(&[&xor, &hash(&[b"Pair-Setup"]), &self.salt, &ap, &bp, &*key]);
        if !bool::from(expected.ct_eq(proof)) {
            return Err(Error::Authentication);
        }
        let server = hash(&[&ap, proof, &*key]);
        Ok((key, server))
    }
}
pub struct PairSetup {
    id: Vec<u8>,
    signing: SigningKey,
    srp: Option<Srp>,
    key: Option<Zeroizing<[u8; 64]>>,
    deadline: u64,
    completed: bool,
}
pub struct Paired {
    pub response: Vec<u8>,
    pub controller_id: Vec<u8>,
    pub controller_key: [u8; 32],
}
impl PairSetup {
    pub fn new(id: Vec<u8>, imported_seed: [u8; 32]) -> Result<Self> {
        if id.is_empty() || id.len() > 128 {
            return Err(Error::Limit);
        }
        Ok(Self {
            id,
            signing: SigningKey::from_bytes(&imported_seed),
            srp: None,
            key: None,
            deadline: 0,
            completed: false,
        })
    }
    pub fn begin(
        &mut self,
        request: &[u8],
        now: u64,
        pairing_window_open: bool,
    ) -> Result<Vec<u8>> {
        if !pairing_window_open || self.completed {
            return Err(Error::State);
        }
        let tlv = tlv8_decode(request)?;
        if tlv.get(&6).map(Vec::as_slice) != Some(&[1]) {
            return Err(Error::State);
        }
        self.key = None;
        let srp = Srp::start();
        let response = tlv8_encode(&[(6, &[2]), (3, &pad(&srp.public)?), (2, &srp.salt)])?;
        self.srp = Some(srp);
        self.deadline = now.saturating_add(60000);
        Ok(response)
    }
    pub fn proof(&mut self, request: &[u8], now: u64) -> Result<Vec<u8>> {
        let srp = self.srp.take().ok_or(Error::State)?;
        if now >= self.deadline {
            return Err(Error::Exhausted);
        }
        let tlv = tlv8_decode(request)?;
        if tlv.get(&6).map(Vec::as_slice) != Some(&[3]) {
            return Err(Error::State);
        }
        let (key, proof) = srp.verify(
            tlv.get(&3).ok_or(Error::Malformed)?,
            tlv.get(&4).ok_or(Error::Malformed)?,
        )?;
        self.key = Some(key);
        tlv8_encode(&[(6, &[4]), (4, &proof)])
    }
    pub fn finish(&mut self, request: &[u8], now: u64) -> Result<Paired> {
        let key = self.key.take().ok_or(Error::State)?;
        if now >= self.deadline {
            return Err(Error::Exhausted);
        }
        let tlv = tlv8_decode(request)?;
        if tlv.get(&6).map(Vec::as_slice) != Some(&[5]) {
            return Err(Error::State);
        }
        let encryption = Zeroizing::new(derive(
            &*key,
            b"Pair-Setup-Encrypt-Salt",
            b"Pair-Setup-Encrypt-Info",
        )?);
        let plain = open(
            &encryption,
            &label(b"PS-Msg05"),
            tlv.get(&5).ok_or(Error::Malformed)?,
            &[],
        )?;
        let sub = tlv8_decode(&plain)?;
        let id = sub.get(&1).ok_or(Error::Malformed)?;
        if id.is_empty() || id.len() > 128 {
            return Err(Error::Limit);
        }
        let pk: [u8; 32] = sub
            .get(&3)
            .ok_or(Error::Malformed)?
            .as_slice()
            .try_into()
            .map_err(|_| Error::Malformed)?;
        let sign_key = Zeroizing::new(derive(
            &*key,
            b"Pair-Setup-Controller-Sign-Salt",
            b"Pair-Setup-Controller-Sign-Info",
        )?);
        let mut msg = sign_key.to_vec();
        msg.extend(id);
        msg.extend(pk);
        let signature = Signature::from_slice(sub.get(&10).ok_or(Error::Malformed)?)
            .map_err(|_| Error::Malformed)?;
        VerifyingKey::from_bytes(&pk)
            .map_err(|_| Error::Authentication)?
            .verify_strict(&msg, &signature)
            .map_err(|_| Error::Authentication)?;
        let own_sign_key = Zeroizing::new(derive(
            &*key,
            b"Pair-Setup-Accessory-Sign-Salt",
            b"Pair-Setup-Accessory-Sign-Info",
        )?);
        let own_pk = self.signing.verifying_key().to_bytes();
        let mut own_msg = own_sign_key.to_vec();
        own_msg.extend(&self.id);
        own_msg.extend(own_pk);
        let sig = self.signing.sign(&own_msg).to_bytes();
        let sub = tlv8_encode(&[(1, &self.id), (3, &own_pk), (10, &sig)])?;
        let encrypted = seal(&encryption, &label(b"PS-Msg06"), &sub, &[])?;
        self.completed = true;
        Ok(Paired {
            response: tlv8_encode(&[(6, &[6]), (5, &encrypted)])?,
            controller_id: id.clone(),
            controller_key: pk,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // Independent SRP client formula. Seeds are synthetic and never persisted.
    fn client(salt: &[u8], public: &[u8]) -> (Vec<u8>, [u8; 64], [u8; 64]) {
        let n = n();
        let g = B::from(5u8);
        let a = B::from_bytes_be(&[42; 32]);
        let ap = pad(&g.modpow(&a, &n)).unwrap();
        let bp = B::from_bytes_be(public);
        let x = B::from_bytes_be(&hash(&[salt, &hash(&[b"Pair-Setup:3939"])]));
        let u = B::from_bytes_be(&hash(&[&ap, public]));
        let k = B::from_bytes_be(&hash(&[&n.to_bytes_be(), &pad(&g).unwrap()]));
        let base = (&bp + &n - ((&k * g.modpow(&x, &n)) % &n)) % &n;
        let s = base.modpow(&(&a + &u * &x), &n);
        let key = hash(&[&s.to_bytes_be()]);
        let mut xor = hash(&[&n.to_bytes_be()]);
        for (v, g) in xor.iter_mut().zip(hash(&[&[5]])) {
            *v ^= g
        }
        let m1 = hash(&[&xor, &hash(&[b"Pair-Setup"]), salt, &ap, public, &key]);
        (ap.to_vec(), m1, key)
    }
    #[test]
    fn srp_proof_matches_independent_client() {
        let s = Srp::with_random([3; 16], Zeroizing::new([7; 32]));
        let (a, m, k) = client(&s.salt, &pad(&s.public).unwrap());
        let (key, m2) = s.verify(&a, &m).unwrap();
        assert_eq!(*key, k);
        assert_eq!(m2, hash(&[&a, &m, &k]));
    }
    #[test]
    fn bad_public_and_proof_fail_closed() {
        for a in [vec![0; 384], pad(&n()).unwrap().to_vec(), vec![1; 384]] {
            assert!(Srp::with_random([3; 16], Zeroizing::new([7; 32]))
                .verify(&a, &[0; 64])
                .is_err())
        }
    }
    #[test]
    fn complete_setup_and_no_replay() {
        let mut setup = PairSetup::new(b"synthetic-accessory".to_vec(), [11; 32]).unwrap();
        let m1 = tlv8_encode(&[(6, &[1])]).unwrap();
        assert!(setup.begin(&m1, 0, false).is_err());
        let m2 = tlv8_decode(&setup.begin(&m1, 0, true).unwrap()).unwrap();
        let (a, proof, key) = client(&m2[&2], &m2[&3]);
        let m3 = tlv8_encode(&[(6, &[3]), (3, &a), (4, &proof)]).unwrap();
        let m4 = tlv8_decode(&setup.proof(&m3, 1).unwrap()).unwrap();
        assert_eq!(m4[&4], hash(&[&a, &proof, &key]));
        let controller = SigningKey::from_bytes(&[33; 32]);
        let pk = controller.verifying_key().to_bytes();
        let id = b"synthetic-phone";
        let sk = derive(
            &key,
            b"Pair-Setup-Controller-Sign-Salt",
            b"Pair-Setup-Controller-Sign-Info",
        )
        .unwrap();
        let mut transcript = sk.to_vec();
        transcript.extend(id);
        transcript.extend(pk);
        let signature = controller.sign(&transcript).to_bytes();
        let sub = tlv8_encode(&[(1, id), (3, &pk), (10, &signature)]).unwrap();
        let ek = derive(&key, b"Pair-Setup-Encrypt-Salt", b"Pair-Setup-Encrypt-Info").unwrap();
        let enc = seal(&ek, &label(b"PS-Msg05"), &sub, &[]).unwrap();
        let m5 = tlv8_encode(&[(6, &[5]), (5, &enc)]).unwrap();
        let paired = setup.finish(&m5, 2).unwrap();
        assert_eq!(paired.controller_key, pk);
        assert_eq!(paired.controller_id, id);
        let m6 = tlv8_decode(&paired.response).unwrap();
        let response = tlv8_decode(&open(&ek, &label(b"PS-Msg06"), &m6[&5], &[]).unwrap()).unwrap();
        let ask = derive(
            &key,
            b"Pair-Setup-Accessory-Sign-Salt",
            b"Pair-Setup-Accessory-Sign-Info",
        )
        .unwrap();
        let mut transcript = ask.to_vec();
        transcript.extend(&response[&1]);
        transcript.extend(&response[&3]);
        let apk: [u8; 32] = response[&3].as_slice().try_into().unwrap();
        VerifyingKey::from_bytes(&apk)
            .unwrap()
            .verify_strict(&transcript, &Signature::from_slice(&response[&10]).unwrap())
            .unwrap();
        assert!(setup.finish(&m5, 3).is_err());
        assert!(setup.begin(&m1, 4, true).is_err());
    }
}
