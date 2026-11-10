//! Bounded APTransportPackage records inside NetSocket ChaCha20-Poly1305 frames.
use crate::{crypto::ControlCipher, Error, Result};
pub struct Tunnel {
    cipher: ControlCipher,
    sealed: Vec<u8>,
    plain: Vec<u8>,
}
impl Tunnel {
    pub fn new(read_key: [u8; 32]) -> Self {
        Self {
            cipher: ControlCipher::new(read_key, [0; 32]),
            sealed: Vec::new(),
            plain: Vec::new(),
        }
    }
    pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        if self.sealed.len() + bytes.len() > 1048576 {
            return Err(Error::Limit);
        }
        self.sealed.extend(bytes);
        let (data, n) = self.cipher.decrypt(&self.sealed)?;
        self.sealed.drain(..n);
        if self.plain.len() + data.len() > 1048576 {
            return Err(Error::Limit);
        }
        self.plain.extend(data);
        let mut offset = 0;
        while self.plain.len() - offset >= 32 {
            let n = u32::from_be_bytes(self.plain[offset..offset + 4].try_into().unwrap()) as usize;
            if !(32..=1048576).contains(&n) {
                return Err(Error::Limit);
            }
            if self.plain.len() - offset < n {
                break;
            }
            if &self.plain[offset + 16..offset + 20] == b"comm" {
                emit(&self.plain[offset + 32..offset + n])?
            }
            offset += n;
        }
        self.plain.drain(..offset);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encrypted_fragmented_packages_and_limits() {
        let mut sender = ControlCipher::new([0; 32], [5; 32]);
        let mut package = vec![0; 32];
        package[..4].copy_from_slice(&35u32.to_be_bytes());
        package[16..20].copy_from_slice(b"comm");
        package.extend(b"iap");
        let wire = sender.encrypt(&package).unwrap();
        let mut tunnel = Tunnel::new([5; 32]);
        let mut received: Vec<u8> = Vec::new();
        for byte in wire.chunks(1) {
            tunnel
                .feed(byte, |b| {
                    received.extend(b);
                    Ok(())
                })
                .unwrap()
        }
        assert_eq!(received, b"iap");
        let invalid = sender.encrypt(&[0; 32]).unwrap();
        assert_eq!(tunnel.feed(&invalid, |_| Ok(())), Err(Error::Limit));
    }
}
