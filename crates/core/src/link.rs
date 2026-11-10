// Minimal wireless iAP2 reliable link, based on PlayPort/DiPlay Iap2LinkEngine.
// GPL-3.0-only. Only the currently required control session 10 is advertised.
use crate::{Error, Result};
use std::collections::VecDeque;
pub const MARKER: [u8; 6] = [0xff, 0x55, 2, 0, 0xee, 0x10];
const BUDGET: usize = 1024 * 1024;
const MAX_PACKET: usize = 65535;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Detecting,
    Negotiating,
    Ready,
    Dead,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Ready,
    Control(Vec<u8>),
    Dead(Error),
}
#[derive(Clone)]
struct Packet {
    seq: u8,
    session: u8,
    data: Vec<u8>,
    deadline: u64,
    retries: u8,
}
#[derive(Clone)]
struct Peer {
    window: usize,
    max_len: usize,
    retry_ms: u64,
    retries: u8,
    control: bool,
    acknowledgements: u8,
}
impl Peer {
    fn decode(b: &[u8]) -> Result<Self> {
        if b.len() < 13 || (b.len() - 10) % 3 != 0 || b[0] != 1 {
            return Err(Error::Malformed);
        }
        let p = Self {
            window: usize::from(b[1]).min(64),
            max_len: u16::from_be_bytes([b[2], b[3]]) as usize,
            retry_ms: u16::from_be_bytes([b[4], b[5]]) as u64,
            retries: b[8].min(8),
            control: b[10..]
                .chunks_exact(3)
                .any(|s| s[0] == 10 && s[1] == 0 && [1, 2].contains(&s[2])),
            acknowledgements: b[9],
        };
        if p.window == 0 || p.max_len <= 10 || !p.control || (p.retries > 0 && p.retry_ms == 0) {
            return Err(Error::Malformed);
        }
        Ok(p)
    }
}
pub struct Link {
    tunnel: bool,
    pub state: State,
    rx: VecDeque<u8>,
    tx: Vec<u8>,
    events: VecDeque<Event>,
    event_bytes: usize,
    queued: VecDeque<Vec<u8>>,
    flight: VecDeque<Packet>,
    reordered: Vec<Packet>,
    pending_bytes: usize,
    seq: u8,
    received: u8,
    peer: Option<Peer>,
    timer: Option<u64>,
    attempt: u8,
}
impl Default for Link {
    fn default() -> Self {
        Self {
            tunnel: false,
            state: State::Idle,
            rx: VecDeque::new(),
            tx: Vec::new(),
            events: VecDeque::new(),
            event_bytes: 0,
            queued: VecDeque::new(),
            flight: VecDeque::new(),
            reordered: Vec::new(),
            pending_bytes: 0,
            seq: 99,
            received: 0,
            peer: None,
            timer: None,
            attempt: 0,
        }
    }
}
fn sum(b: &[u8]) -> u8 {
    b.iter().fold(0u8, |n, b| n.wrapping_add(*b))
}
fn wire(flags: u8, seq: u8, ack: u8, session: u8, payload: Option<&[u8]>) -> Vec<u8> {
    let len = 9 + payload.map_or(0, |b| b.len() + 1);
    let mut out = vec![
        0xff,
        0x5a,
        (len >> 8) as u8,
        len as u8,
        flags,
        seq,
        ack,
        session,
    ];
    out.push(0u8.wrapping_sub(sum(&out)));
    if let Some(b) = payload {
        out.extend(b);
        out.push(0u8.wrapping_sub(sum(b)));
    }
    out
}
impl Link {
    pub fn start(&mut self, now: u64) -> Result<()> {
        if self.state != State::Idle {
            return Err(Error::State);
        }
        self.state = State::Detecting;
        self.timer = Some(now.saturating_add(1000));
        self.append(&MARKER)
    }
    pub fn start_tunnel(&mut self, now: u64) -> Result<()> {
        self.start(now)?;
        self.tunnel = true;
        self.state = State::Negotiating;
        self.sync()?;
        self.timer = Some(now.saturating_add(500));
        Ok(())
    }
    pub fn eof(&mut self) {
        self.die(Error::Truncated)
    }
    fn die(&mut self, e: Error) {
        if self.state == State::Dead {
            return;
        }
        self.state = State::Dead;
        self.timer = None;
        self.rx.clear();
        self.tx.clear();
        self.queued.clear();
        self.flight.clear();
        self.reordered.clear();
        self.pending_bytes = 0;
        self.events.clear();
        self.event_bytes = 0;
        self.events.push_back(Event::Dead(e));
    }
    fn append(&mut self, b: &[u8]) -> Result<()> {
        if self.tx.len() + b.len() > BUDGET {
            self.die(Error::Limit);
            return Err(Error::Limit);
        }
        self.tx.extend(b);
        Ok(())
    }
    fn emit(&mut self, e: Event) -> Result<()> {
        let size = match &e {
            Event::Control(b) => b.len(),
            _ => 0,
        };
        if self.events.len() >= 256 || self.event_bytes + size > BUDGET {
            self.die(Error::Limit);
            return Err(Error::Limit);
        }
        self.event_bytes += size;
        self.events.push_back(e);
        Ok(())
    }
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx)
    }
    pub fn event(&mut self) -> Option<Event> {
        let e = self.events.pop_front()?;
        if let Event::Control(b) = &e {
            self.event_bytes -= b.len()
        }
        Some(e)
    }
    pub fn deadline(&self) -> Option<u64> {
        self.timer
            .into_iter()
            .chain(self.flight.iter().map(|p| p.deadline))
            .min()
    }
    fn sync(&mut self) -> Result<()> {
        self.append(&wire(
            0x80,
            self.seq,
            self.received,
            0,
            Some(if self.tunnel {
                &[1, 4, 255, 255, 0, 0, 0, 0, 0, 0, 10, 0, 2]
            } else {
                &[1, 4, 255, 255, 15, 160, 1, 244, 4, 3, 10, 0, 2]
            }),
        ))
    }
    fn ack(&mut self) -> Result<()> {
        self.append(&wire(0x40, self.seq, self.received, 0, None))
    }
    pub fn feed(&mut self, bytes: &[u8], now: u64) -> Result<()> {
        if matches!(self.state, State::Idle | State::Dead) {
            return Err(Error::State);
        }
        if bytes.len() > MAX_PACKET {
            return Err(Error::Limit);
        }
        for b in bytes {
            if self.rx.len() >= MAX_PACKET {
                self.die(Error::Limit);
                return Err(Error::Limit);
            }
            self.rx.push_back(*b);
            if let Err(e) = self.drain(now) {
                self.die(e);
                return Err(e);
            }
        }
        Ok(())
    }
    fn drain(&mut self, now: u64) -> Result<()> {
        loop {
            if self.state == State::Detecting {
                if self.rx.len() < 6 {
                    return Ok(());
                }
                let marker: Vec<u8> = self.rx.drain(..6).collect();
                if marker != MARKER {
                    return Err(Error::Malformed);
                }
                self.state = State::Negotiating;
                self.attempt = 0;
                self.sync()?;
                self.timer = Some(now.saturating_add(500));
            }
            while self.rx.len() >= 2 && (self.rx[0] != 0xff || self.rx[1] != 0x5a) {
                self.rx.pop_front();
            }
            if self.rx.len() < 9 {
                return Ok(());
            }
            let header: Vec<u8> = self.rx.iter().take(9).copied().collect();
            if sum(&header) != 0 {
                self.rx.pop_front();
                continue;
            }
            let n = u16::from_be_bytes([header[2], header[3]]) as usize;
            if n < 9 {
                self.rx.pop_front();
                continue;
            }
            if self.rx.len() < n {
                return Ok(());
            }
            let packet: Vec<u8> = self.rx.drain(..n).collect();
            let payload = if n == 9 {
                None
            } else {
                if sum(&packet[9..]) != 0 {
                    continue;
                }
                Some(&packet[9..n - 1])
            };
            self.frame(&header, payload, now)?;
        }
    }
    fn frame(&mut self, h: &[u8], payload: Option<&[u8]>, now: u64) -> Result<()> {
        let flags = h[4];
        if flags & 0x10 != 0 {
            return Err(Error::State);
        }
        if flags & 0x80 != 0 {
            if self.state == State::Negotiating {
                self.peer = Some(Peer::decode(payload.ok_or(Error::Malformed)?)?);
                self.received = h[5];
                self.ack()?;
            } else if self.state == State::Ready {
                self.ack()?;
            }
        }
        if flags & 0x40 != 0 {
            if self.state == State::Negotiating {
                if self.peer.is_none() || h[6] != self.seq {
                    return Ok(());
                }
                self.state = State::Ready;
                self.timer = None;
                self.emit(Event::Ready)?;
            }
            if self.state == State::Ready {
                // Never accept an ACK of an unsent sequence. Wraparound is handled by exact flight membership.
                if let Some(at) = self.flight.iter().position(|p| p.seq == h[6]) {
                    for _ in 0..=at {
                        let p = self.flight.pop_front().unwrap();
                        self.pending_bytes -= p.data.len();
                    }
                }
                self.flush(now)?;
            }
        }
        if flags & 0x20 != 0 && self.state == State::Ready {
            if let Some(missing) = payload {
                let ids: Vec<u8> = self
                    .flight
                    .iter()
                    .filter(|p| missing.contains(&p.seq))
                    .map(|p| p.seq)
                    .collect();
                for seq in ids {
                    self.resend(seq, now)?;
                }
            }
        }
        if flags & !0x40 == 0 && self.state == State::Ready {
            if let Some(b) = payload {
                let distance = h[5].wrapping_sub(self.received);
                if distance == 0 || distance > 8 {
                    self.ack()?;
                    return Ok(());
                }
                if !self.reordered.iter().any(|p| p.seq == h[5]) {
                    if self.reordered.len() >= 8 {
                        return Err(Error::Limit);
                    }
                    self.reordered.push(Packet {
                        seq: h[5],
                        session: h[7],
                        data: b.to_vec(),
                        deadline: 0,
                        retries: 0,
                    });
                }
                while let Some(at) = self
                    .reordered
                    .iter()
                    .position(|p| p.seq == self.received.wrapping_add(1))
                {
                    let p = self.reordered.remove(at);
                    self.received = p.seq;
                    if p.session == 10 {
                        self.emit(Event::Control(p.data))?;
                    }
                }
                if self.peer.as_ref().is_some_and(|p| p.acknowledgements == 0) {
                    return Ok(());
                }
                // Immediate cumulative ACKs remain within negotiated acknowledgement deadlines.
                self.ack()?;
                if distance > 1 {
                    let mut missing = Vec::new();
                    let mut seq = self.received.wrapping_add(1);
                    while seq != h[5] && missing.len() < 8 {
                        if !self.reordered.iter().any(|p| p.seq == seq) {
                            missing.push(seq)
                        }
                        seq = seq.wrapping_add(1);
                    }
                    if !missing.is_empty() {
                        self.append(&wire(0x20, self.seq, self.received, 0, Some(&missing)))?;
                    }
                }
            }
        }
        Ok(())
    }
    pub fn send_control(&mut self, b: &[u8], now: u64) -> Result<()> {
        if self.state != State::Ready {
            return Err(Error::State);
        }
        let max = self.peer.as_ref().unwrap().max_len - 10;
        let count = b.len().div_ceil(max);
        if self.queued.len() + self.flight.len() + count > 64
            || self.pending_bytes + b.len() > BUDGET
        {
            return Err(Error::Limit);
        }
        for c in b.chunks(max) {
            self.queued.push_back(c.to_vec());
            self.pending_bytes += c.len()
        }
        self.flush(now)
    }
    fn flush(&mut self, now: u64) -> Result<()> {
        let peer = self.peer.as_ref().ok_or(Error::State)?.clone();
        while self.flight.len() < peer.window {
            let Some(data) = self.queued.pop_front() else {
                break;
            };
            self.seq = self.seq.wrapping_add(1);
            self.append(&wire(0x40, self.seq, self.received, 10, Some(&data)))?;
            if peer.retries == 0 {
                self.pending_bytes -= data.len()
            } else {
                self.flight.push_back(Packet {
                    seq: self.seq,
                    session: 10,
                    data,
                    deadline: now.saturating_add(peer.retry_ms),
                    retries: 0,
                });
            }
        }
        Ok(())
    }
    fn resend(&mut self, seq: u8, now: u64) -> Result<()> {
        let peer = self.peer.as_ref().ok_or(Error::State)?.clone();
        let packet = {
            let p = self
                .flight
                .iter_mut()
                .find(|p| p.seq == seq)
                .ok_or(Error::State)?;
            p.retries += 1;
            if p.retries >= peer.retries {
                return Err(Error::Exhausted);
            }
            p.deadline = now.saturating_add(peer.retry_ms);
            wire(0x40, p.seq, self.received, p.session, Some(&p.data))
        };
        self.append(&packet)
    }
    pub fn advance(&mut self, now: u64) -> Result<()> {
        if self.state == State::Dead {
            return Err(Error::State);
        }
        let result = (|| {
            if self.timer.is_some_and(|t| t <= now) {
                self.attempt += 1;
                if self.attempt >= 10 {
                    return Err(Error::Exhausted);
                }
                match self.state {
                    State::Detecting => {
                        self.append(&MARKER)?;
                        self.timer = Some(now.saturating_add(1000));
                    }
                    State::Negotiating => {
                        self.sync()?;
                        self.timer = Some(now.saturating_add(500));
                    }
                    _ => self.timer = None,
                }
            }
            let due: Vec<u8> = self
                .flight
                .iter()
                .filter(|p| p.deadline <= now)
                .map(|p| p.seq)
                .collect();
            for seq in due {
                self.resend(seq, now)?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.die(e)
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> (Link, Link) {
        let (mut a, mut b) = (Link::default(), Link::default());
        a.start(0).unwrap();
        b.start(0).unwrap();
        for _ in 0..4 {
            let x = a.take_output();
            for c in x.chunks(3) {
                b.feed(c, 0).unwrap()
            }
            let y = b.take_output();
            for c in y.chunks(2) {
                a.feed(c, 0).unwrap()
            }
        }
        assert_eq!(a.state, State::Ready);
        assert_eq!(b.state, State::Ready);
        assert_eq!(a.event(), Some(Event::Ready));
        assert_eq!(b.event(), Some(Event::Ready));
        (a, b)
    }
    #[test]
    fn tunnel_profile_matches_upstream_and_does_not_wait_for_acks() {
        let (mut a, mut b) = (Link::default(), Link::default());
        a.start_tunnel(0).unwrap();
        b.start_tunnel(0).unwrap();
        let syn = a.take_output();
        assert_eq!(&syn[..6], &MARKER);
        assert_eq!(&syn[15..28], &[1, 4, 255, 255, 0, 0, 0, 0, 0, 0, 10, 0, 2]);
        b.feed(&syn, 0).unwrap();
        a.feed(&b.take_output(), 0).unwrap();
        b.feed(&a.take_output(), 0).unwrap();
        assert_eq!(a.state, State::Ready);
        assert_eq!(b.state, State::Ready);
        a.event();
        b.event();
        for _ in 0..100 {
            a.send_control(b"csm", 1).unwrap();
            b.feed(&a.take_output(), 1).unwrap();
            assert_eq!(b.event(), Some(Event::Control(b"csm".to_vec())));
            assert!(b.take_output().is_empty());
        }
        assert!(a.flight.is_empty());
        assert_eq!(a.deadline(), None);
    }
    #[test]
    fn handshake_fragmentation_wrap_and_ack() {
        let (mut a, mut b) = pair();
        for n in 0..300u16 {
            a.send_control(&n.to_be_bytes(), 1).unwrap();
            let w = a.take_output();
            b.feed(&w, 1).unwrap();
            assert_eq!(b.event(), Some(Event::Control(n.to_be_bytes().to_vec())));
            a.feed(&b.take_output(), 1).unwrap();
            assert!(a.flight.is_empty())
        }
    }
    #[test]
    fn lost_packet_retransmit_and_duplicate() {
        let (mut a, mut b) = pair();
        a.send_control(b"one", 1).unwrap();
        a.take_output();
        a.advance(4001).unwrap();
        let resent = a.take_output();
        b.feed(&resent, 4001).unwrap();
        b.feed(&resent, 4001).unwrap();
        assert_eq!(b.event(), Some(Event::Control(b"one".to_vec())));
        assert_eq!(b.event(), None);
        a.feed(&b.take_output(), 4001).unwrap();
        assert_eq!(a.deadline(), None);
    }
    #[test]
    fn out_of_order_delivered_once() {
        let (mut a, mut b) = pair();
        a.send_control(b"one", 1).unwrap();
        let one = a.take_output();
        a.send_control(b"two", 1).unwrap();
        let two = a.take_output();
        b.feed(&two, 1).unwrap();
        assert_eq!(b.event(), None);
        b.feed(&one, 1).unwrap();
        assert_eq!(b.event(), Some(Event::Control(b"one".to_vec())));
        assert_eq!(b.event(), Some(Event::Control(b"two".to_vec())));
    }
    #[test]
    fn bounded_failure_and_eof() {
        let (mut a, _) = pair();
        a.send_control(b"lost", 0).unwrap();
        for t in [4000, 8000, 12000] {
            a.advance(t).unwrap();
            a.take_output();
        }
        assert_eq!(a.advance(16000), Err(Error::Exhausted));
        assert_eq!(a.state, State::Dead);
        assert_eq!(a.deadline(), None);
        assert_eq!(a.pending_bytes, 0);
        let mut l = Link::default();
        l.start(0).unwrap();
        l.eof();
        assert_eq!(l.event(), Some(Event::Dead(Error::Truncated)));
    }
    #[test]
    fn invalid_peer_and_checksum() {
        let (mut a, mut b) = pair();
        a.send_control(b"bytes", 0).unwrap();
        let mut p = a.take_output();
        p[9] ^= 1;
        b.feed(&p, 0).unwrap();
        assert_eq!(b.event(), None);
        assert!(Peer::decode(&[1, 0, 0, 9, 0, 0, 0, 0, 0, 0, 10, 0, 1]).is_err());
    }
}
