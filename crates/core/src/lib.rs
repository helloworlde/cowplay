//! Platform-independent, bounded protocol components. No platform I/O.
pub mod crypto;
pub mod framing;
pub mod media;
pub mod session;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    Malformed,
    Limit,
    Authentication,
    State,
    Exhausted,
}
pub type Result<T> = std::result::Result<T, Error>;
pub mod bplist;
pub mod control;
pub mod info;
pub mod link;
pub mod mfi;
pub mod pairing;
pub mod rtsp;
pub mod setup;
pub mod wireless;

pub mod audio;

pub mod bootstrap;
pub mod tunnel;

pub mod validation;
#[cfg(feature = "tesla")]
pub mod tesla;
pub mod vehicle;
pub mod vehicle_status;
