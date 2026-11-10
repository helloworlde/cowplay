// Adapted protocol behavior from PlayPort AirPlayCrypto / ControlCipher (GPL-3.0-only).
use crate::{Error, Result};
use chacha20poly1305::{
    aead::{Aead, AeadInPlace, Payload},
    ChaCha20Poly1305, KeyInit,
};
use zeroize::Zeroize;
pub fn nonce(counter: u64) -> [u8; 12] {
    let mut n = [0; 12];
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}
pub fn derive(shared: &[u8], salt: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    let mut k = [0; 32];
    hkdf::Hkdf::<sha2::Sha512>::new(Some(salt), shared)
        .expand(info, &mut k)
        .map_err(|_| Error::Limit)?;
    Ok(k)
}
pub fn seal(key: &[u8; 32], n: &[u8; 12], plain: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    ChaCha20Poly1305::new(key.into())
        .encrypt(n.into(), Payload { msg: plain, aad })
        .map_err(|_| Error::Authentication)
}
pub fn open(key: &[u8; 32], n: &[u8; 12], cipher: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    ChaCha20Poly1305::new(key.into())
        .decrypt(n.into(), Payload { msg: cipher, aad })
        .map_err(|_| Error::Authentication)
}
pub fn open_in_place(key: &[u8; 32], n: &[u8; 12], body: &mut Vec<u8>, aad: &[u8]) -> Result<()> {
    ChaCha20Poly1305::new(key.into())
        .decrypt_in_place(n.into(), aad, body)
        .map_err(|_| Error::Authentication)
}
pub struct ControlCipher {
    read: [u8; 32],
    write: [u8; 32],
    rx: u64,
    tx: u64,
    failed: bool,
}
impl Drop for ControlCipher {
    fn drop(&mut self) {
        self.read.zeroize();
        self.write.zeroize();
    }
}
impl ControlCipher {
    pub fn new(read: [u8; 32], write: [u8; 32]) -> Self {
        Self {
            read,
            write,
            rx: 0,
            tx: 0,
            failed: false,
        }
    }
    pub fn encrypt(&mut self, plain: &[u8]) -> Result<Vec<u8>> {
        if self.failed {
            return Err(Error::State);
        }
        if plain.len() > 1024 * 1024 {
            return Err(Error::Limit);
        }
        let mut out = Vec::new();
        for chunk in plain.chunks(16384) {
            let next = self.tx.checked_add(1).ok_or(Error::Exhausted)?;
            let h = (chunk.len() as u16).to_le_bytes();
            out.extend(h);
            out.extend(seal(&self.write, &nonce(self.tx), chunk, &h)?);
            self.tx = next;
        }
        Ok(out)
    }
    // Caller retains incomplete bytes; bad MAC permanently closes this cipher.
    pub fn decrypt(&mut self, input: &[u8]) -> Result<(Vec<u8>, usize)> {
        if self.failed {
            return Err(Error::State);
        }
        if input.len() > 1024 * 1024 {
            return Err(Error::Limit);
        }
        let mut at = 0;
        let mut out = Vec::new();
        while input.len() - at >= 2 {
            let h = &input[at..at + 2];
            let n = u16::from_le_bytes([h[0], h[1]]) as usize;
            if n > 16384 {
                self.failed = true;
                return Err(Error::Limit);
            }
            if input.len() - at < n + 18 {
                break;
            }
            let next = self.rx.checked_add(1).ok_or(Error::Exhausted)?;
            match open(&self.read, &nonce(self.rx), &input[at + 2..at + n + 18], h) {
                Ok(b) => out.extend(b),
                Err(e) => {
                    self.failed = true;
                    return Err(e);
                }
            }
            self.rx = next;
            at += n + 18;
        }
        Ok((out, at))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_and_tampered() {
        let mut a = ControlCipher::new([1; 32], [2; 32]);
        let mut b = ControlCipher::new([2; 32], [1; 32]);
        let plain = vec![5; 20000];
        let wire = a.encrypt(&plain).unwrap();
        assert_eq!(b.decrypt(&wire[..4]).unwrap(), (vec![], 0));
        assert_eq!(b.decrypt(&wire).unwrap(), (plain, wire.len()));
        let mut bad = a.encrypt(b"next").unwrap();
        bad[3] ^= 1;
        assert_eq!(b.decrypt(&bad), Err(Error::Authentication));
        assert_eq!(b.decrypt(&wire), Err(Error::State));
    }
    #[test]
    fn reject_oversized() {
        assert_eq!(
            ControlCipher::new([0; 32], [0; 32]).decrypt(&[255, 255]),
            Err(Error::Limit)
        );
    }
}
