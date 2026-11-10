// MFi v3 P-256 and auth-setup behavior from PlayPort/DiPlay; GPL-3.0-only.
// Matching a key and certificate does not establish Apple trust or licensing.
use crate::{
    framing::{params, Csm},
    Error, Result,
};
use ctr::cipher::{KeyIvInit, StreamCipher};
use der::{Decode, Encode};
use p256::{
    ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey, VerifyingKey},
    pkcs8::{DecodePrivateKey, DecodePublicKey},
};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;
pub trait Identity: Send + Sync {
    fn certificate(&self) -> &[u8];
    fn sign_digest(&self, digest: &[u8; 32]) -> Result<[u8; 64]>;
}
pub struct ImportedIdentity {
    key: SigningKey,
    certificate: Vec<u8>,
}
impl ImportedIdentity {
    pub fn from_pkcs8_and_certificate(pkcs8: &[u8], blob: &[u8]) -> Result<Self> {
        if pkcs8.len() > 16384 || blob.len() > 16384 || blob.is_empty() {
            return Err(Error::Limit);
        }
        let key = SigningKey::from_pkcs8_der(pkcs8).map_err(|_| Error::Authentication)?;
        let cert = match x509_cert::Certificate::from_der(blob) {
            Ok(c) => c,
            Err(_) => {
                let info =
                    cms::content_info::ContentInfo::from_der(blob).map_err(|_| Error::Malformed)?;
                if info.content_type.to_string() != "1.2.840.113549.1.7.2" {
                    return Err(Error::Malformed);
                }
                let signed = cms::signed_data::SignedData::from_der(
                    &info.content.to_der().map_err(|_| Error::Malformed)?,
                )
                .map_err(|_| Error::Malformed)?;
                let certs = signed.certificates.ok_or(Error::Malformed)?;
                if certs.0.len() != 1 {
                    return Err(Error::Malformed);
                }
                match certs.0.iter().next().ok_or(Error::Malformed)? {
                    cms::cert::CertificateChoices::Certificate(c) => c.clone(),
                    _ => return Err(Error::Malformed),
                }
            }
        };
        let spki = cert
            .tbs_certificate
            .subject_public_key_info
            .to_der()
            .map_err(|_| Error::Malformed)?;
        let public = VerifyingKey::from_public_key_der(&spki).map_err(|_| Error::Authentication)?;
        if &public != key.verifying_key() {
            return Err(Error::Authentication);
        }
        Ok(Self {
            key,
            certificate: blob.to_vec(),
        })
    }
}
impl Identity for ImportedIdentity {
    fn certificate(&self) -> &[u8] {
        &self.certificate
    }
    fn sign_digest(&self, digest: &[u8; 32]) -> Result<[u8; 64]> {
        let sig: Signature = self
            .key
            .sign_prehash(digest)
            .map_err(|_| Error::Authentication)?;
        Ok(sig.to_bytes().into())
    }
}
pub fn parameter(id: u16, value: &[u8]) -> Result<Vec<u8>> {
    if value.len() > 65525 {
        return Err(Error::Limit);
    }
    let mut b = ((value.len() + 4) as u16).to_be_bytes().to_vec();
    b.extend(id.to_be_bytes());
    b.extend(value);
    Ok(b)
}
#[derive(Debug, PartialEq, Eq)]
pub enum AuthState {
    Certificate,
    Challenge,
    Confirmation,
    Accepted,
    Failed,
}
pub struct IapAuthentication {
    pub state: AuthState,
    deadline: u64,
    requests: u8,
}
impl IapAuthentication {
    pub fn new(now: u64) -> Self {
        Self {
            state: AuthState::Certificate,
            deadline: now.saturating_add(30000),
            requests: 0,
        }
    }
    pub fn advance(&mut self, now: u64) -> Result<()> {
        if !matches!(self.state, AuthState::Accepted | AuthState::Failed) && now >= self.deadline {
            self.state = AuthState::Failed;
            return Err(Error::Exhausted);
        }
        Ok(())
    }
    pub fn handle(
        &mut self,
        frame: &Csm,
        identity: &dyn Identity,
        now: u64,
    ) -> Result<Option<Csm>> {
        let result = (|| {
            self.advance(now)?;
            if matches!(self.state, AuthState::Accepted | AuthState::Failed) {
                return Err(Error::State);
            }
            self.requests += 1;
            if self.requests > 8 {
                return Err(Error::Limit);
            }
            match frame.id {
                0xaa00 if matches!(self.state, AuthState::Certificate | AuthState::Challenge) => {
                    self.state = AuthState::Challenge;
                    Ok(Some(Csm {
                        id: 0xaa01,
                        body: parameter(0, identity.certificate())?,
                    }))
                }
                0xaa02 if self.state == AuthState::Challenge => {
                    let p = params(&frame.body)?;
                    if p.len() != 1 || p[0].0 != 0 {
                        return Err(Error::Malformed);
                    }
                    let digest: [u8; 32] = p[0].1.try_into().map_err(|_| Error::Malformed)?;
                    let signature = identity.sign_digest(&digest)?;
                    self.state = AuthState::Confirmation;
                    Ok(Some(Csm {
                        id: 0xaa03,
                        body: parameter(0, &signature)?,
                    }))
                }
                0xaa05 if self.state == AuthState::Confirmation => {
                    self.state = AuthState::Accepted;
                    Ok(None)
                }
                0xaa04 => Err(Error::Authentication),
                _ => Err(Error::State),
            }
        })();
        if result.is_err() {
            self.state = AuthState::Failed
        }
        result
    }
}
pub fn auth_setup(body: &[u8], identity: &dyn Identity) -> Result<Vec<u8>> {
    if body.len() != 33 || body[0] != 1 {
        return Err(Error::Malformed);
    }
    let peer: [u8; 32] = body[1..].try_into().unwrap();
    let secret = StaticSecret::random_from_rng(rand_core::OsRng);
    let own = PublicKey::from(&secret).to_bytes();
    let shared = secret.diffie_hellman(&PublicKey::from(peer));
    if !shared.was_contributory() {
        return Err(Error::Authentication);
    }
    let mut h = Sha1::new();
    h.update(b"AES-KEY");
    h.update(shared.as_bytes());
    let k = Zeroizing::new(h.finalize().to_vec());
    let mut h = Sha1::new();
    h.update(b"AES-IV");
    h.update(shared.as_bytes());
    let iv = Zeroizing::new(h.finalize().to_vec());
    let mut digest = Sha256::new();
    digest.update(own);
    digest.update(peer);
    let mut signature = identity.sign_digest(&digest.finalize().into())?.to_vec();
    ctr::Ctr128BE::<aes::Aes128>::new_from_slices(&k[..16], &iv[..16])
        .map_err(|_| Error::Authentication)?
        .apply_keystream(&mut signature);
    let cert = identity.certificate();
    if cert.is_empty() || cert.len() > 16384 {
        return Err(Error::Limit);
    }
    let mut out = own.to_vec();
    out.extend((cert.len() as u32).to_be_bytes());
    out.extend(cert);
    out.extend((signature.len() as u32).to_be_bytes());
    out.extend(signature);
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::hazmat::PrehashVerifier;
    struct Synthetic(SigningKey);
    impl Identity for Synthetic {
        fn certificate(&self) -> &[u8] {
            b"SYNTHETIC TEST ONLY, NOT A CERTIFICATE"
        }
        fn sign_digest(&self, d: &[u8; 32]) -> Result<[u8; 64]> {
            let sig: Signature = self.0.sign_prehash(d).unwrap();
            Ok(sig.to_bytes().into())
        }
    }
    fn synthetic() -> Synthetic {
        Synthetic(SigningKey::from_bytes((&[7; 32]).into()).unwrap())
    }
    #[test]
    fn csm_auth_requires_certificate_challenge_then_phone_acceptance() {
        let id = synthetic();
        let mut auth = IapAuthentication::new(0);
        assert_eq!(
            auth.handle(
                &Csm {
                    id: 0xaa00,
                    body: vec![]
                },
                &id,
                1
            )
            .unwrap()
            .unwrap()
            .id,
            0xaa01
        );
        let f = auth
            .handle(
                &Csm {
                    id: 0xaa02,
                    body: parameter(0, &[5; 32]).unwrap(),
                },
                &id,
                2,
            )
            .unwrap()
            .unwrap();
        let pars = params(&f.body).unwrap();
        id.0.verifying_key()
            .verify_prehash(&[5; 32], &Signature::from_slice(pars[0].1).unwrap())
            .unwrap();
        assert_eq!(auth.state, AuthState::Confirmation);
        auth.handle(
            &Csm {
                id: 0xaa05,
                body: vec![],
            },
            &id,
            3,
        )
        .unwrap();
        assert_eq!(auth.state, AuthState::Accepted);
        let mut early = IapAuthentication::new(0);
        assert!(early
            .handle(
                &Csm {
                    id: 0xaa05,
                    body: vec![]
                },
                &id,
                1
            )
            .is_err());
    }
    #[test]
    fn sap_response_decrypts_and_binds_both_ephemeral_keys() {
        let id = synthetic();
        let secret = StaticSecret::from([8; 32]);
        let public = PublicKey::from(&secret).to_bytes();
        let mut req = vec![1];
        req.extend(public);
        let response = auth_setup(&req, &id).unwrap();
        let own: [u8; 32] = response[..32].try_into().unwrap();
        let shared = secret.diffie_hellman(&PublicKey::from(own));
        let certlen = u32::from_be_bytes(response[32..36].try_into().unwrap()) as usize;
        assert_eq!(&response[36..36 + certlen], id.certificate());
        let mut sig = response[40 + certlen..].to_vec();
        let k = Sha1::digest([b"AES-KEY".as_slice(), shared.as_bytes()].concat());
        let iv = Sha1::digest([b"AES-IV".as_slice(), shared.as_bytes()].concat());
        ctr::Ctr128BE::<aes::Aes128>::new_from_slices(&k[..16], &iv[..16])
            .unwrap()
            .apply_keystream(&mut sig);
        let digest = Sha256::digest([own, public].concat());
        id.0.verifying_key()
            .verify_prehash(&digest, &Signature::from_slice(&sig).unwrap())
            .unwrap();
        assert!(auth_setup(&[1; 32], &id).is_err());
    }
    #[test]
    fn identity_import_rejects_fake_material() {
        assert!(ImportedIdentity::from_pkcs8_and_certificate(
            b"not private key",
            b"not certificate"
        )
        .is_err());
    }
}
