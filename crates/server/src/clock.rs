//! Media-domain NTP synchronization. Never adjusts the operating-system clock.
use std::time::{Instant, SystemTime, UNIX_EPOCH};
pub struct Clock {
    start: Instant,
    base: u64,
    offset: i64,
    pending: Option<u64>,
    best_rtt: u64,
    samples: u8,
    synced: bool,
}
impl Default for Clock {
    fn default() -> Self {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            start: Instant::now(),
            base: ((d.as_secs() + 2208988800) << 32)
                | (((d.subsec_nanos() as u64) << 32) / 1_000_000_000),
            offset: 0,
            pending: None,
            best_rtt: u64::MAX,
            samples: 0,
            synced: false,
        }
    }
}
impl Clock {
    pub fn local(&self) -> u64 {
        self.base
            .wrapping_add(((self.start.elapsed().as_nanos() << 32) / 1_000_000_000) as u64)
    }
    pub fn now(&self) -> u64 {
        self.local().wrapping_add_signed(self.offset)
    }
    pub fn synced(&self) -> bool {
        self.synced
    }
    pub fn request(&mut self) -> u64 {
        let t = self.local();
        self.pending = Some(t);
        t
    }
    pub fn response(&mut self, t1: u64, t2: u64, t3: u64, t4: u64) -> bool {
        if self.pending != Some(t1) {
            return false;
        }
        self.pending = None;
        let diff = |a: u64, b: u64| a.wrapping_sub(b) as i64 as i128;
        let rtt = diff(t4, t1) - diff(t3, t2);
        if rtt < 0 || rtt > (1i128 << 32) {
            return false;
        }
        let offset = (diff(t2, t1) + diff(t3, t4)) / 2;
        if offset.abs() > (86400i128 << 32) {
            return false;
        }
        crate::diag::diag().phone_rtt(((rtt as u128 * 1_000_000) >> 32) as u64);
        // Retain the lowest-delay observation in each bounded window.
        if !self.synced || rtt as u64 <= self.best_rtt {
            self.offset = offset as i64;
            self.best_rtt = rtt as u64;
            self.synced = true
        }
        self.samples += 1;
        if self.samples == 8 {
            self.samples = 0;
            self.best_rtt = u64::MAX
        }
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_rtt_and_offset() {
        let mut c = Clock::default();
        let t = c.request();
        let unit = 1u64 << 32;
        assert!(!c.response(t + 1, t, t, t));
        assert!(c.response(t, t + unit / 4, t + unit / 4, t + unit / 8));
        assert_eq!(c.offset, (3 * unit / 16) as i64);
        assert!(c.synced());
        assert!(!c.response(t, t, t, t));
        let t = c.request();
        assert!(!c.response(t, t, t, t + 2 * unit));
    }
}
