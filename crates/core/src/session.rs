use crate::{Error, Result};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Stopped,
    WaitingPlatform,
    Authenticating,
    Streaming,
    Backoff,
    Failed,
}
pub struct Session {
    pub state: State,
    attempt: u8,
    deadline: u64,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            state: State::Stopped,
            attempt: 0,
            deadline: 0,
        }
    }
}
impl Session {
    pub fn start(&mut self) {
        if self.state == State::Stopped {
            self.state = State::WaitingPlatform;
            self.attempt = 0
        }
    }
    pub fn transport_ready(&mut self, now: u64) -> Result<()> {
        if self.state != State::WaitingPlatform {
            return Err(Error::State);
        }
        self.state = State::Authenticating;
        self.deadline = now.saturating_add(15000);
        Ok(())
    }
    pub fn authenticated(&mut self) -> Result<()> {
        if self.state != State::Authenticating {
            return Err(Error::State);
        }
        self.state = State::Streaming;
        self.attempt = 0;
        Ok(())
    }
    pub fn fail(&mut self, now: u64) {
        if matches!(self.state, State::Stopped | State::Failed) {
            return;
        }
        self.attempt += 1;
        if self.attempt >= 5 {
            self.state = State::Failed
        } else {
            self.state = State::Backoff;
            self.deadline = now.saturating_add(1000u64 << self.attempt);
        }
    }
    pub fn advance(&mut self, now: u64) {
        if now < self.deadline {
            return;
        }
        match self.state {
            State::Authenticating => self.fail(now),
            State::Backoff => self.state = State::WaitingPlatform,
            _ => {}
        }
    }
    pub fn stop(&mut self) {
        *self = Self::default()
    }
    pub fn deadline(&self) -> Option<u64> {
        if matches!(self.state, State::Authenticating | State::Backoff) {
            Some(self.deadline)
        } else {
            None
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_retries_and_stop() {
        let mut s = Session::default();
        assert!(s.authenticated().is_err());
        s.start();
        s.start();
        for _ in 0..5 {
            s.fail(0);
            s.advance(999999);
        }
        assert_eq!(s.state, State::Failed);
        assert_eq!(s.deadline(), None);
        s.stop();
        s.advance(u64::MAX);
        assert_eq!(s.state, State::Stopped);
    }
}
