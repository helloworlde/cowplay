use std::time::{Duration, Instant};

pub type RuntimeResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub mod tesla_ble;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Open,
    Wpa2,
    Wpa3,
}

pub fn handoff_security(encryption: &str) -> Option<Security> {
    match encryption.split('+').next().unwrap_or("") {
        "none" => Some(Security::Open),
        "psk" | "psk2" | "psk-mixed" | "sae-mixed" => Some(Security::Wpa2),
        "sae" => Some(Security::Wpa3),
        _ => None,
    }
}

pub fn handoff_security_code(encryption: &str) -> Option<u8> {
    match handoff_security(encryption) {
        Some(Security::Open) => Some(0),
        Some(Security::Wpa2) => Some(2),
        Some(Security::Wpa3) => Some(4),
        None => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    Confirm { request: u64, passkey: u32 },
    Pairing,
    Paired,
    Cancelled,
    Expired,
}

impl Phase {
    /// Stable name shown to pages and diagnostics; never includes the passkey.
    pub fn name(self) -> &'static str {
        match self {
            Phase::Waiting => "Waiting",
            Phase::Confirm { .. } => "Confirm",
            Phase::Pairing => "Pairing",
            Phase::Paired => "Paired",
            Phase::Cancelled => "Cancelled",
            Phase::Expired => "Expired",
        }
    }
}

pub struct PairingWindow {
    opened: Instant,
    deadline: Instant,
    phase: Phase,
    sequence: u64,
}

impl PairingWindow {
    pub fn new(now: Instant) -> Self {
        Self {
            opened: now,
            deadline: now + Duration::from_secs(180),
            phase: Phase::Waiting,
            sequence: 0,
        }
    }
    pub fn phase(&mut self, now: Instant) -> Phase {
        if now >= self.deadline
            && matches!(
                self.phase,
                Phase::Waiting | Phase::Confirm { .. } | Phase::Pairing
            )
        {
            self.phase = Phase::Expired;
        }
        self.phase
    }
    pub fn request_confirmation(
        &mut self,
        now: Instant,
        passkey: u32,
    ) -> Result<u64, &'static str> {
        if self.phase(now) != Phase::Waiting {
            return Err("pairing window is not waiting");
        }
        if passkey > 999_999 {
            return Err("invalid Bluetooth passkey");
        }
        self.deadline = self
            .deadline
            .max((now + Duration::from_secs(120)).min(self.opened + Duration::from_secs(600)));
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("request sequence exhausted")?;
        self.phase = Phase::Confirm {
            request: self.sequence,
            passkey,
        };
        Ok(self.sequence)
    }
    pub fn confirm(
        &mut self,
        now: Instant,
        request: u64,
        accept: bool,
    ) -> Result<(), &'static str> {
        if !matches!(self.phase(now), Phase::Confirm { request: current, .. } if current == request)
        {
            return Err("pairing request changed or expired");
        }
        self.phase = if accept {
            Phase::Pairing
        } else {
            Phase::Cancelled
        };
        Ok(())
    }
    pub fn bluez_paired(&mut self, now: Instant) -> Result<(), &'static str> {
        if self.phase(now) != Phase::Pairing {
            return Err("no confirmed pairing in progress");
        }
        self.phase = Phase::Paired;
        Ok(())
    }
    /// Still waiting for an iPhone, showing a code, or finishing the bond.
    pub fn is_open(&mut self, now: Instant) -> bool {
        matches!(self.phase(now), Phase::Waiting | Phase::Confirm { .. } | Phase::Pairing)
    }
    /// Time left before the window closes.
    pub fn remaining(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }
    pub fn cancel(&mut self) {
        self.phase = Phase::Cancelled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transition_security_is_wpa2_and_enterprise_is_rejected() {
        assert_eq!(handoff_security("sae-mixed+ccmp"), Some(Security::Wpa2));
        assert_eq!(handoff_security("sae"), Some(Security::Wpa3));
        for value in ["wpa2", "owe", "", "unknown"] {
            assert_eq!(handoff_security(value), None);
        }
    }
    #[test]
    fn late_attempt_gets_time_to_confirm() {
        let now = Instant::now();
        let mut w = PairingWindow::new(now);
        let id = w
            .request_confirmation(now + Duration::from_secs(175), 42)
            .unwrap();
        w.confirm(now + Duration::from_secs(200), id, true).unwrap();
        assert_eq!(w.phase(now + Duration::from_secs(201)), Phase::Pairing);
        w.bluez_paired(now + Duration::from_secs(202)).unwrap();
        assert_eq!(w.phase(now + Duration::from_secs(600)), Phase::Paired);
    }
    #[test]
    fn stale_confirmation_and_timeout_cannot_pair() {
        let now = Instant::now();
        let mut w = PairingWindow::new(now);
        let id = w.request_confirmation(now, 123456).unwrap();
        assert!(w.confirm(now, id + 1, true).is_err());
        assert!(w.confirm(now + Duration::from_secs(181), id, true).is_err());
        assert!(w.bluez_paired(now + Duration::from_secs(181)).is_err());
    }
    #[test]
    fn cancel_and_rejection_are_terminal() {
        for accept in [false, true] {
            let now = Instant::now();
            let mut w = PairingWindow::new(now);
            let id = w.request_confirmation(now, 999999).unwrap();
            w.confirm(now, id, accept).unwrap();
            if accept {
                w.cancel();
            }
            assert_eq!(w.phase(now), Phase::Cancelled);
            assert!(w.bluez_paired(now).is_err());
        }
    }
    #[test]
    fn phase_name_hides_the_passkey_and_open_covers_live_phases() {
        let now = Instant::now();
        let mut w = PairingWindow::new(now);
        assert!(w.is_open(now));
        let id = w.request_confirmation(now, 123456).unwrap();
        assert_eq!(w.phase(now).name(), "Confirm");
        assert!(w.is_open(now));
        w.confirm(now, id, true).unwrap();
        assert!(w.is_open(now));
        w.bluez_paired(now).unwrap();
        assert!(!w.is_open(now));
        assert_eq!(w.phase(now).name(), "Paired");
        assert!(!PairingWindow::new(now).is_open(now + Duration::from_secs(181)));
    }
    #[test]
    fn no_automatic_pairing_or_duplicate_request() {
        let now = Instant::now();
        let mut w = PairingWindow::new(now);
        assert!(w.bluez_paired(now).is_err());
        assert!(w.request_confirmation(now, 1000000).is_err());
        w.request_confirmation(now, 0).unwrap();
        assert!(w.request_confirmation(now, 1).is_err());
        assert!(w.bluez_paired(now).is_err());
    }
}

pub mod acme;
pub mod admin;
mod persistent_logs;
mod auto_fps;
pub mod console;
pub mod ctl;
pub use platform::openwrt::{network, uci};
pub mod policy;
pub mod bluetooth;
pub mod platform;
pub use platform::f50 as board;
pub mod import;
pub mod rfcomm;
pub mod service;
pub mod web;

#[cfg(test)]
mod wire_security_tests {
    #[test]
    fn actual_iap2_security_values() {
        assert_eq!(super::handoff_security_code("sae"), Some(4));
        assert_eq!(super::handoff_security_code("sae-mixed+ccmp"), Some(2));
        assert_eq!(super::handoff_security_code("none"), Some(0));
        assert_eq!(super::handoff_security_code("owe"), None);
    }
}
pub mod image;

pub mod certificates;
mod identity;

pub mod phone_flow;

pub mod clients_compat;
pub mod updater;
