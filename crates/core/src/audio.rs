//! AirPlay RTP audio framing derived from PlayPort AudioStream. No decoding/transcoding.
use crate::{crypto::open, Error, Result};

/// RTP/ChaCha20-Poly1305 counters for one microphone uplink.
#[derive(Default, Debug, Clone)]
pub struct MicCounters {
    pub sequence: u16,
    pub timestamp: u32,
    pub nonce: u64,
}
/// Seal one big-endian PCM microphone frame (PlayPort/DiPlay `MicrophonePacketizer`):
/// clear 12-byte RTP header, AAD = header[4..12], nonce = 4 zero bytes + LE64 counter, and the
/// 8-byte LE counter appended after the Poly1305 tag.
pub fn seal_mic_packet(key: &[u8; 32], payload_type: u8, c: &mut MicCounters, body: &[u8], samples: u32) -> Result<Vec<u8>> {
    let mut header = [0u8; 12];
    header[0] = 0x80;
    header[1] = payload_type & 0x7f;
    header[2..4].copy_from_slice(&c.sequence.to_be_bytes());
    header[4..8].copy_from_slice(&c.timestamp.to_be_bytes());
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&c.nonce.to_le_bytes());
    let sealed = crate::crypto::seal(key, &nonce, body, &header[4..12])?;
    let mut packet = Vec::with_capacity(12 + sealed.len() + 8);
    packet.extend_from_slice(&header);
    packet.extend_from_slice(&sealed);
    packet.extend_from_slice(&c.nonce.to_le_bytes());
    c.sequence = c.sequence.wrapping_add(1);
    c.timestamp = c.timestamp.wrapping_add(samples);
    c.nonce += 1;
    Ok(packet)
}
#[cfg(test)]
mod mic_tests {
    #[test]
    fn mic_packet_layout_matches_playport() {
        let key = [7u8; 32];
        let mut c = super::MicCounters { sequence: 0xfffe, timestamp: 10, nonce: 5 };
        let body = [1u8, 2, 3, 4];
        let p = super::seal_mic_packet(&key, 100, &mut c, &body, 320).unwrap();
        assert_eq!(&p[..4], &[0x80, 100, 0xff, 0xfe]);
        assert_eq!(&p[4..8], &10u32.to_be_bytes());
        assert_eq!(p.len(), 12 + 4 + 16 + 8);
        assert_eq!(&p[p.len() - 8..], &5u64.to_le_bytes());
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&5u64.to_le_bytes());
        let plain = crate::crypto::open(&key, &nonce, &p[12..p.len() - 8], &p[4..12]).unwrap();
        assert_eq!(plain, body);
        assert_eq!((c.sequence, c.timestamp, c.nonce), (0xffff, 330, 6));
    }
}
use zeroize::Zeroizing;
#[derive(Clone, Copy, Debug)]
pub struct Format {
    pub codec: u8,
    pub rate: u32,
    pub channels: u8,
}
impl Format {
    pub fn from_bits(bits: u64) -> Result<Self> {
        let (codec, rate, channels) = match bits {
            0x400000 => (1, 44100, 2),
            0x800000 => (1, 48000, 2),
            0x10000000 | 0x20000000 | 0x40000000 => (2, 48000, 1),
            4 => (3, 8000, 1),
            8 => (3, 8000, 2),
            16 => (3, 16000, 1),
            32 => (3, 16000, 2),
            64 => (3, 24000, 1),
            128 => (3, 24000, 2),
            256 => (3, 32000, 1),
            512 => (3, 32000, 2),
            1024 => (3, 44100, 1),
            2048 => (3, 44100, 2),
            16384 => (3, 48000, 1),
            32768 => (3, 48000, 2),
            _ => return Err(Error::Malformed),
        };
        Ok(Self {
            codec,
            rate,
            channels,
        })
    }
    pub fn config(&self, kind: u8, name: &str) -> Result<Vec<u8>> {
        if !(100..=102).contains(&kind) || name.len() > 64 || !name.is_ascii() {
            return Err(Error::Malformed);
        }
        let mut b = vec![3, kind, self.codec];
        b.extend(self.rate.to_le_bytes());
        b.extend([self.channels, name.len() as u8]);
        b.extend(name.as_bytes());
        if self.codec == 1 {
            let index = if self.rate == 48000 { 3 } else { 4 };
            let asc = (2u16 << 11) | (index << 7) | ((self.channels as u16) << 3);
            b.extend(asc.to_be_bytes());
        }
        Ok(b)
    }
}
pub struct Decoder {
    key: Zeroizing<[u8; 32]>,
    format: Format,
    highest: Option<u64>,
    seen: u64,
    last_sample: Option<u32>,
    samples: u64,
}
impl Decoder {
    pub fn new(key: [u8; 32], format: Format) -> Self {
        Self {
            key: Zeroizing::new(key),
            format,
            highest: None,
            seen: 0,
            last_sample: None,
            samples: 0,
        }
    }
    pub fn decode(&mut self, wire: &[u8], kind: u8) -> Result<Option<Vec<u8>>> {
        if wire.len() < 36 || wire.len() > 4096 || wire[0] != 0x80 || !(100..=102).contains(&kind) {
            return Err(Error::Malformed);
        }
        let end = wire.len() - 8;
        let counter = u64::from_le_bytes(wire[end..].try_into().unwrap());
        // Authenticate before changing replay/timestamp state. Nonce is authenticated by AEAD.
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(&wire[end..]);
        let mut data = open(&self.key, &nonce, &wire[12..end], &wire[4..12])?;
        if let Some(high) = self.highest {
            if counter <= high {
                let back = high - counter;
                if back >= 64 || self.seen & (1 << back) != 0 {
                    return Ok(None);
                }
                self.seen |= 1 << back;
            } else {
                let d = counter - high;
                self.seen = if d >= 64 { 1 } else { (self.seen << d) | 1 };
                self.highest = Some(counter);
            }
        } else {
            self.highest = Some(counter);
            self.seen = 1;
        }
        let sample = u32::from_be_bytes(wire[4..8].try_into().unwrap());
        if let Some(last) = self.last_sample {
            let delta = sample.wrapping_sub(last);
            if delta >= 0x80000000 {
                return Ok(None);
            }
            self.samples = self
                .samples
                .checked_add(delta as u64)
                .ok_or(Error::Exhausted)?;
        }
        self.last_sample = Some(sample);
        if self.format.codec == 3 {
            if data.len() % (2 * self.format.channels as usize) != 0 {
                return Err(Error::Malformed);
            }
            for s in data.chunks_exact_mut(2) {
                s.swap(0, 1);
            }
        }
        let timestamp = self
            .samples
            .checked_mul(1_000_000)
            .ok_or(Error::Exhausted)?
            / self.format.rate as u64;
        let mut out = vec![4, kind];
        out.extend(timestamp.to_le_bytes());
        out.extend(data);
        Ok(Some(out))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{nonce, seal};
    fn packet(sample: u32, counter: u64) -> Vec<u8> {
        let mut b = vec![0x80, 96, 0, 1];
        b.extend(sample.to_be_bytes());
        b.extend([0, 0, 0, 1]);
        let c = seal(&[9; 32], &nonce(counter), &[0x12, 0x34], &b[4..12]).unwrap();
        b.extend(c);
        b.extend(counter.to_le_bytes());
        b
    }
    #[test]
    fn rtp_auth_replay_timestamp_wrap_and_pcm() {
        let mut d = Decoder::new([9; 32], Format::from_bits(0x4000).unwrap());
        let p = packet(u32::MAX - 47, 0);
        let a = d.decode(&p, 101).unwrap().unwrap();
        assert_eq!(&a[10..], &[0x34, 0x12]);
        assert!(d.decode(&p, 101).unwrap().is_none());
        let b = d.decode(&packet(0, 1), 101).unwrap().unwrap();
        assert_eq!(u64::from_le_bytes(b[2..10].try_into().unwrap()), 1000);
        let mut bad = packet(48, 2);
        bad[15] ^= 1;
        assert_eq!(d.decode(&bad, 101), Err(Error::Authentication));
        assert!(d.decode(&packet(48, 2), 101).unwrap().is_some());
    }
    #[test]
    fn strict_format_and_aac_description() {
        assert!(Format::from_bits(0x400004).is_err());
        let c = Format::from_bits(0x800000)
            .unwrap()
            .config(102, "media")
            .unwrap();
        assert_eq!(&c[c.len() - 2..], &[0x11, 0x90]);
    }
}
