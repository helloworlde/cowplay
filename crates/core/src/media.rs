// PlayPort ScreenStream and Wire protocol behavior, GPL-3.0-only. Strict malformed-input rejection.
use crate::{crypto, Error, Result};
use std::collections::VecDeque;
pub const MAX_MEDIA: usize = 8 * 1024 * 1024;
/// Locked PlayPort four-byte navigation report. Never clamp malformed browser input.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnobState {
    pub select: bool, pub home: bool, pub back: bool,
    pub x: i8, pub y: i8, pub wheel: i8,
}
pub fn knob_report(k: &KnobState) -> Result<[u8; 4]> {
    if [k.x, k.y, k.wheel].contains(&i8::MIN) { return Err(Error::Malformed); }
    Ok([u8::from(k.select) | u8::from(k.home) << 1 | u8::from(k.back) << 2,
        k.x as u8, k.y as u8, k.wheel as u8])
}
pub enum ScreenMessage {
    Config(Vec<u8>),
    Frame(Vec<u8>),
    Control,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenPacketKind {
    Config,
    Frame,
    Control,
}
pub struct ScreenDecoder {
    key: [u8; 32],
    counter: u64,
}
impl Drop for ScreenDecoder {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.key.zeroize();
    }
}
impl ScreenDecoder {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key, counter: 0 }
    }
    pub fn decode(&mut self, header: &[u8], body: &[u8]) -> Result<ScreenMessage> {
        let mut bytes = body.to_vec();
        Ok(match self.decode_in_place(header, &mut bytes)? {
            ScreenPacketKind::Config => ScreenMessage::Config(bytes),
            ScreenPacketKind::Frame => ScreenMessage::Frame(bytes),
            ScreenPacketKind::Control => ScreenMessage::Control,
        })
    }
    pub fn decode_in_place(
        &mut self,
        header: &[u8],
        body: &mut Vec<u8>,
    ) -> Result<ScreenPacketKind> {
        if header.len() != 128 {
            return Err(Error::Malformed);
        }
        let n = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        if n > MAX_MEDIA {
            return Err(Error::Limit);
        }
        if n != body.len() {
            return Err(Error::Truncated);
        }
        match header[4] {
            1 => {
                if n > 65536 {
                    return Err(Error::Limit);
                }
                Ok(ScreenPacketKind::Config)
            }
            0 => {
                if n < 16 {
                    return Err(Error::Authentication);
                }
                let next = self.counter.checked_add(1).ok_or(Error::Exhausted)?;
                crypto::open_in_place(&self.key, &crypto::nonce(self.counter), body, header)?;
                self.counter = next;
                annex_b_in_place(body)?;
                Ok(ScreenPacketKind::Frame)
            }
            // The observed empty opcode-2 record carries no video and consumes no nonce.
            2 if n == 0 => Ok(ScreenPacketKind::Control),
            _ => Err(Error::Malformed),
        }
    }
}
/// Convert four-byte length-prefixed AVCC/HVCC NALs. Input format is fixed by the screen protocol.
pub fn annex_b_in_place(b: &mut [u8]) -> Result<()> {
    if b.len() > MAX_MEDIA {
        return Err(Error::Limit);
    }
    let mut at = 0;
    while at < b.len() {
        if b.len() - at < 4 {
            return Err(Error::Truncated);
        }
        let n = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        if n == 0 || n > b.len() - at - 4 {
            return Err(Error::Malformed);
        }
        b[at..at + 4].copy_from_slice(&[0, 0, 0, 1]);
        at += 4 + n;
    }
    Ok(())
}

pub fn annex_b(b: &[u8]) -> Result<Vec<u8>> {
    if b.len() > MAX_MEDIA {
        return Err(Error::Limit);
    }
    let mut out = Vec::with_capacity(b.len());
    let mut at = 0;
    while at < b.len() {
        if b.len() - at < 4 {
            return Err(Error::Truncated);
        }
        let n = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        at += 4;
        if n == 0 || n > b.len() - at {
            return Err(Error::Malformed);
        }
        out.extend([0, 0, 0, 1]);
        out.extend(&b[at..at + n]);
        at += n;
    }
    Ok(out)
}
pub fn is_keyframe(b: &[u8]) -> bool {
    b.windows(4).any(|w| w[..3] == [0, 0, 1] && w[3] & 31 == 5)
}
/// HEVC random-access point: IRAP NAL unit types 16..=23 (BLA/IDR/CRA).
pub fn is_keyframe_hevc(b: &[u8]) -> bool {
    b.windows(4).any(|w| w[..3] == [0, 0, 1] && (16..=23).contains(&((w[3] >> 1) & 0x3f)))
}
#[cfg(test)]
mod keyframe_tests {
    #[test]
    fn hevc_and_h264_keyframes_are_distinguished() {
        assert!(super::is_keyframe(&[0, 0, 0, 1, 0x65, 1]));
        assert!(super::is_keyframe_hevc(&[0, 0, 0, 1, 19 << 1, 1]));
        assert!(super::is_keyframe_hevc(&[0, 0, 0, 1, 21 << 1, 1]));
        assert!(!super::is_keyframe_hevc(&[0, 0, 0, 1, 1 << 1, 1]));
    }
}
pub fn video_wire(data: &[u8], pts_us: u64, key: bool) -> Vec<u8> {
    let mut b = vec![2, 110, u8::from(key)];
    b.extend(pts_us.to_le_bytes());
    b.extend(data);
    b
}
pub fn video_wire_owned(mut data: Vec<u8>, pts_us: u64, key: bool) -> Vec<u8> {
    let n = data.len();
    data.resize(n + 11, 0);
    data.copy_within(0..n, 11);
    data[..3].copy_from_slice(&[2, 110, u8::from(key)]);
    data[3..11].copy_from_slice(&pts_us.to_le_bytes());
    data
}
#[derive(Clone)]
pub struct Frame {
    pub data: Vec<u8>,
    pub pts_us: u64,
    pub key: bool,
}
pub struct VideoQueue {
    frames: VecDeque<Frame>,
    bytes: usize,
    max_bytes: usize,
    max_frames: usize,
    waiting: bool,
    pub dropped: u64,
}
impl VideoQueue {
    pub fn new(max_bytes: usize, max_frames: usize) -> Self {
        Self {
            frames: VecDeque::new(),
            bytes: 0,
            max_bytes,
            max_frames,
            waiting: true,
            dropped: 0,
        }
    }
    pub fn discontinuity(&mut self) {
        self.dropped += self.frames.len() as u64;
        self.frames.clear();
        self.bytes = 0;
        self.waiting = true;
    }
    fn admit(&mut self, len: usize, key: bool) -> bool {
        if len > self.max_bytes || self.max_frames == 0 {
            self.discontinuity();
            self.dropped += 1;
            return false;
        }
        if self.bytes + len > self.max_bytes || self.frames.len() >= self.max_frames {
            self.discontinuity();
        }
        if self.waiting && !key {
            self.dropped += 1;
            return false;
        }
        if key {
            self.waiting = false
        }
        true
    }
    pub fn push(&mut self, f: Frame) -> bool {
        if !self.admit(f.data.len(), f.key) {
            return false;
        }
        self.bytes += f.data.len();
        self.frames.push_back(f);
        true
    }
    pub fn push_reusable(&mut self, data: &mut Vec<u8>, pts_us: u64, key: bool) -> bool {
        if !self.admit(data.len(), key) {
            return false;
        }
        let f = Frame {
            data: std::mem::take(data),
            pts_us,
            key,
        };
        self.bytes += f.data.len();
        self.frames.push_back(f);
        true
    }

    pub fn pop(&mut self) -> Option<Frame> {
        let f = self.frames.pop_front()?;
        self.bytes -= f.data.len();
        Some(f)
    }
    pub fn needs_keyframe(&self) -> bool {
        self.waiting
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}
#[derive(serde::Deserialize)]
pub struct Contact {
    pub id: u8,
    pub x: f64,
    pub y: f64,
    pub down: bool,
}
pub fn touch_report(c: &[Contact], width: u16, height: u16) -> Result<[u8; 12]> {
    if c.len() > 2 || width == 0 || height == 0 {
        return Err(Error::Malformed);
    }
    let mut b = [0; 12];
    b[6] = 1;
    let mut seen = 0;
    for p in c {
        if p.id > 1
            || seen & (1 << p.id) != 0
            || !p.x.is_finite()
            || !p.y.is_finite()
            || !(0.0..=1.0).contains(&p.x)
            || !(0.0..=1.0).contains(&p.y)
        {
            return Err(Error::Malformed);
        }
        seen |= 1 << p.id;
        let o = p.id as usize * 6;
        b[o + 1] = u8::from(p.down);
        b[o + 2..o + 4].copy_from_slice(&((p.x * (width - 1) as f64).round() as u16).to_le_bytes());
        b[o + 4..o + 6]
            .copy_from_slice(&((p.y * (height - 1) as f64).round() as u16).to_le_bytes());
    }
    Ok(b)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_control_packet_keeps_video_nonce_and_frames() {
        let mut decoder = ScreenDecoder::new([9; 32]);
        let mut frame = [0; 128];
        frame[..4].copy_from_slice(&22u32.to_le_bytes());
        let sealed = |counter| {
            crypto::seal(
                &[9; 32],
                &crypto::nonce(counter),
                &[0, 0, 0, 2, 0x65, 1],
                &frame,
            )
            .unwrap()
        };
        assert!(matches!(
            decoder.decode(&frame, &sealed(0)).unwrap(),
            ScreenMessage::Frame(_)
        ));
        let mut control = [0; 128];
        control[4] = 2;
        assert!(matches!(
            decoder.decode(&control, &[]).unwrap(),
            ScreenMessage::Control
        ));
        assert!(matches!(
            decoder.decode(&frame, &sealed(1)).unwrap(),
            ScreenMessage::Frame(_)
        ));
        control[..4].copy_from_slice(&1u32.to_le_bytes());
        assert!(decoder.decode(&control, &[0]).is_err());
        control[..4].fill(0);
        control[4] = 3;
        assert!(decoder.decode(&control, &[]).is_err());
    }
    #[test]
    fn dropped_frame_keeps_storage_and_nonce_progress() {
        let mut decoder = ScreenDecoder::new([9; 32]);
        let mut q = VideoQueue::new(64, 2);
        for n in 0..3 {
            let mut header = [0; 128];
            header[..4].copy_from_slice(&22u32.to_le_bytes());
            let mut data =
                crypto::seal(&[9; 32], &crypto::nonce(n), &[0, 0, 0, 2, 0x41, 1], &header).unwrap();
            let ptr = data.as_ptr();
            let capacity = data.capacity();
            assert_eq!(
                decoder.decode_in_place(&header, &mut data).unwrap(),
                ScreenPacketKind::Frame
            );
            assert_eq!(data.as_ptr(), ptr);
            assert!(!q.push_reusable(&mut data, 0, false));
            assert_eq!(data.as_ptr(), ptr);
            assert_eq!(data.capacity(), capacity);
            assert_eq!(q.bytes(), 0);
        }
        let mut data = Vec::with_capacity(32);
        data.extend_from_slice(&[0, 0, 0, 1, 0x65, 1]);
        let ptr = data.as_ptr();
        let wire = video_wire_owned(data, 42, true);
        assert_eq!(wire.as_ptr(), ptr);
        assert_eq!(wire.len(), 17);
    }
    #[test]
    fn congestion_requires_new_idr() {
        let mut q = VideoQueue::new(10, 2);
        let f = |key| Frame {
            data: vec![1; 4],
            pts_us: 0,
            key,
        };
        assert!(!q.push(f(false)));
        assert!(q.push(f(true)));
        assert!(q.push(f(false)));
        assert!(!q.push(f(false)));
        assert!(q.needs_keyframe());
        assert_eq!(q.bytes(), 0);
        assert!(q.push(f(true)));
    }
    #[test]
    fn encrypted_avcc_length_prefix_cannot_be_mistaken_for_annex_b() {
        for len in [1usize, 256, 258, 511] {
            let mut plain = (len as u32).to_be_bytes().to_vec();
            plain.extend(std::iter::repeat_n(0x41, len));
            plain.extend_from_slice(&[0, 0, 0, 2, 0x65, 1]);
            let mut expected = plain.clone();
            expected[..4].copy_from_slice(&[0, 0, 0, 1]);
            expected[4 + len..8 + len].copy_from_slice(&[0, 0, 0, 1]);
            let mut header = [0u8; 128];
            header[..4].copy_from_slice(&((plain.len() + 16) as u32).to_le_bytes());
            let mut body = crypto::seal(&[9; 32], &crypto::nonce(0), &plain, &header).unwrap();
            let ptr = body.as_ptr();
            assert_eq!(ScreenDecoder::new([9; 32]).decode_in_place(&header, &mut body).unwrap(), ScreenPacketKind::Frame);
            assert_eq!(body, expected, "first AVCC NAL length={len}");
            assert_eq!(body.as_ptr(), ptr, "conversion must stay in place");
            assert!(is_keyframe(&body), "the second NAL is IDR");
            assert_eq!(annex_b(&plain).unwrap(), expected);
        }
    }
    #[test]
    fn media_mac_and_nals() {
        let mut h = [0; 128];
        h[..4].copy_from_slice(&22u32.to_le_bytes());
        let b = crypto::seal(&[9; 32], &crypto::nonce(0), &[0, 0, 0, 2, 0x65, 1], &h).unwrap();
        match ScreenDecoder::new([9; 32]).decode(&h, &b).unwrap() {
            ScreenMessage::Frame(v) => assert_eq!(v, vec![0, 0, 0, 1, 0x65, 1]),
            _ => panic!(),
        };
        assert!(ScreenDecoder::new([8; 32]).decode(&h, &b).is_err());
        assert!(annex_b(&[0, 0, 0, 5, 1]).is_err());
    }
    #[test]
    fn touches_checked() {
        assert!(touch_report(
            &[Contact {
                id: 0,
                x: f64::NAN,
                y: 0.,
                down: true
            }],
            800,
            480
        )
        .is_err());
        assert_eq!(
            touch_report(&[], 800, 480).unwrap(),
            [0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0]
        );
    }
}
