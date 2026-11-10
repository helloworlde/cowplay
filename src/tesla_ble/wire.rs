//! Tesla's two-byte big-endian length prefix and 1024-byte message limit.
//! Based on the official vehicle-command BLE connector, not vehicle state decoding.
use std::{
    io,
    time::{Duration, Instant},
};
const MAX: usize = 1024;
const CHUNK_GAP: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct Frames {
    partial: Vec<u8>,
    last: Option<Instant>,
}
impl Frames {
    /// A gap over one second discards only the incomplete old message.
    /// Notifications and messages may have arbitrary, different boundaries.
    pub fn receive(&mut self, chunk: &[u8], now: Instant) -> io::Result<Vec<Vec<u8>>> {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) > CHUNK_GAP)
        {
            self.partial.clear();
        }
        self.last = Some(now);
        let mut complete = Vec::new();
        for byte in chunk {
            self.partial.push(*byte);
            if self.partial.len() < 2 {
                continue;
            }
            let length = u16::from_be_bytes([self.partial[0], self.partial[1]]) as usize;
            if length > MAX {
                self.partial.clear();
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid Tesla BLE message length",
                ));
            }
            if self.partial.len() == length + 2 {
                complete.push(self.partial.split_off(2));
                self.partial.clear();
            }
        }
        Ok(complete)
    }

    /// Conservative ATT payload size; a higher negotiated MTU is optional.
    pub fn chunks(message: &[u8], payload_size: usize) -> io::Result<Vec<Vec<u8>>> {
        if message.len() > MAX || !(20..=512).contains(&payload_size) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid Tesla BLE packet size",
            ));
        }
        let mut frame = Vec::with_capacity(message.len() + 2);
        frame.extend_from_slice(&(message.len() as u16).to_be_bytes());
        frame.extend_from_slice(message);
        Ok(frame.chunks(payload_size).map(<[u8]>::to_vec).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_split_boundaries_and_combined_packets_preserve_messages() {
        let now = Instant::now();
        let a: Vec<_> = (0..=255).cycle().take(MAX).collect();
        let b = vec![7; 33];
        let mut bytes = Frames::chunks(&a, 20).unwrap().concat();
        bytes.extend(Frames::chunks(&b, 20).unwrap().concat());
        for split in 0..bytes.len() {
            let mut frames = Frames::default();
            let mut result = frames.receive(&bytes[..split], now).unwrap();
            result.extend(
                frames
                    .receive(&bytes[split..], now + Duration::from_millis(1))
                    .unwrap(),
            );
            assert_eq!(result, vec![a.clone(), b.clone()], "split {split}");
            assert!(frames.partial.is_empty());
        }
    }
    #[test]
    fn expired_partial_is_discarded_and_new_message_can_arrive() {
        let now = Instant::now();
        let mut frames = Frames::default();
        assert!(frames.receive(&[0, 5, 1], now).unwrap().is_empty());
        assert_eq!(
            frames
                .receive(&[0, 2, 8, 9], now + Duration::from_millis(1001))
                .unwrap(),
            vec![vec![8, 9]]
        );
    }
    #[test]
    fn invalid_lengths_do_not_leave_a_poisoned_buffer() {
        let now = Instant::now();
        let mut frames = Frames::default();
        for length in [1025, u16::MAX] {
            assert!(frames.receive(&length.to_be_bytes(), now).is_err());
            assert!(frames.partial.is_empty());
            assert_eq!(frames.receive(&[0, 1, 9], now).unwrap(), vec![vec![9]]);
        }
        assert_eq!(
            frames.receive(&[0, 0], now).unwrap(),
            vec![Vec::<u8>::new()]
        );
        assert_eq!(Frames::chunks(&[], 20).unwrap(), vec![vec![0, 0]]);
        assert!(Frames::chunks(&vec![0; 1025], 20).is_err());
        assert!(Frames::chunks(&[1], 0).is_err());
    }
}
