// Protocol reference: PlayPort / DiPlay iAP2 CSM framing. GPL-3.0-only.
use crate::{Error, Result};
use std::collections::VecDeque;
pub const MAX_FRAME: usize = 65535;
#[derive(Debug, PartialEq, Eq)]
pub struct Csm {
    pub id: u16,
    pub body: Vec<u8>,
}
impl Csm {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let len = self.body.len().checked_add(6).ok_or(Error::Limit)?;
        if len > MAX_FRAME {
            return Err(Error::Limit);
        }
        let mut b = vec![0x40, 0x40];
        b.extend((len as u16).to_be_bytes());
        b.extend(self.id.to_be_bytes());
        b.extend(&self.body);
        Ok(b)
    }
}
#[derive(Default)]
pub struct CsmDecoder {
    buf: VecDeque<u8>,
}
impl CsmDecoder {
    // Callback avoids an unbounded result list for large batches.
    pub fn feed(&mut self, input: &[u8], mut emit: impl FnMut(Csm)) -> Result<()> {
        if input.len() > MAX_FRAME {
            return Err(Error::Limit);
        }
        for byte in input {
            self.buf.push_back(*byte);
            while self.buf.len() >= 2 && (self.buf[0] != 0x40 || self.buf[1] != 0x40) {
                self.buf.pop_front();
            }
            if self.buf.len() < 6 {
                continue;
            }
            let n = u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize;
            if n < 6 {
                self.buf.pop_front();
                continue;
            }
            if self.buf.len() == n {
                let id = u16::from_be_bytes([self.buf[4], self.buf[5]]);
                self.buf.drain(..6);
                let body = self.buf.drain(..).collect();
                emit(Csm { id, body });
            }
        }
        Ok(())
    }
    pub fn eof(&mut self) -> Result<()> {
        let incomplete = !self.buf.is_empty();
        self.buf.clear();
        if incomplete {
            Err(Error::Truncated)
        } else {
            Ok(())
        }
    }
}
pub fn params(mut b: &[u8]) -> Result<Vec<(u16, &[u8])>> {
    if b.len() > MAX_FRAME - 6 {
        return Err(Error::Limit);
    }
    let mut out = Vec::new();
    while !b.is_empty() {
        if b.len() < 4 {
            return Err(Error::Truncated);
        }
        let n = u16::from_be_bytes([b[0], b[1]]) as usize;
        if n < 4 {
            return Err(Error::Malformed);
        }
        if n > b.len() {
            return Err(Error::Truncated);
        }
        if out.len() >= 256 {
            return Err(Error::Limit);
        }
        out.push((u16::from_be_bytes([b[2], b[3]]), &b[4..n]));
        b = &b[n..];
    }
    Ok(out)
}
pub fn tlv8_decode(mut b: &[u8]) -> Result<std::collections::BTreeMap<u8, Vec<u8>>> {
    if b.len() > 65535 {
        return Err(Error::Limit);
    }
    let mut out = std::collections::BTreeMap::<u8, Vec<u8>>::new();
    while !b.is_empty() {
        if b.len() < 2 {
            return Err(Error::Truncated);
        }
        let n = b[1] as usize;
        if b.len() < n + 2 {
            return Err(Error::Truncated);
        }
        out.entry(b[0]).or_default().extend(&b[2..2 + n]);
        b = &b[2 + n..];
    }
    Ok(out)
}
pub fn tlv8_encode(items: &[(u8, &[u8])]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for (k, v) in items {
        if v.is_empty() {
            out.extend([*k, 0]);
        }
        for c in v.chunks(255) {
            if out.len() + c.len() + 2 > 65535 {
                return Err(Error::Limit);
            }
            out.extend([*k, c.len() as u8]);
            out.extend(c);
        }
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragments_and_repeated_parameters() {
        let f = Csm {
            id: 0xaa00,
            body: vec![0, 4, 0, 1, 0, 4, 0, 1],
        };
        let encoded = f.encode().unwrap();
        for split in 0..encoded.len() {
            let mut d = CsmDecoder::default();
            let mut frames = vec![];
            d.feed(&encoded[..split], |x| frames.push(x)).unwrap();
            d.feed(&encoded[split..], |x| frames.push(x)).unwrap();
            assert_eq!(
                frames,
                vec![Csm {
                    id: f.id,
                    body: f.body.clone()
                }]
            );
            assert_eq!(params(&frames[0].body).unwrap().len(), 2);
            d.eof().unwrap();
        }
    }
    #[test]
    fn invalid_lengths_and_disconnect() {
        assert_eq!(params(&[0, 0, 0, 1]), Err(Error::Malformed));
        assert_eq!(params(&[0, 8, 0, 1]), Err(Error::Truncated));
        let mut d = CsmDecoder::default();
        d.feed(&[0x40, 0x40, 0, 8], |_| panic!()).unwrap();
        assert_eq!(d.eof(), Err(Error::Truncated));
    }
    #[test]
    fn tlv_fragments() {
        let data = vec![7; 600];
        let v = tlv8_encode(&[(1, &data)]).unwrap();
        assert_eq!(tlv8_decode(&v).unwrap()[&1], data);
        assert!(tlv8_decode(&[1, 9, 0]).is_err());
    }
}
