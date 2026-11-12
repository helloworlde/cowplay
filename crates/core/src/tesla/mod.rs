//! Tesla vehicle-command telemetry and limited HVAC. Sources pinned in research/tesla-ble-lock.json.
//! P-256/ring perform cryptography; vehicle commands are restricted to HVAC power and temperature.
mod climate;
mod proto;
mod session;
pub use climate::{ClimateCommand, ClimateState};
use proto::Message;
pub use session::{Client, EnrollmentPermission, Identity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Malformed,
    Authentication,
    NotAuthorized,
    VehicleFault(u32),
    Expired,
    State,
    Exhausted,
    Resync,
    Rejected,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tesla protocol: {self:?}")
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
pub use crate::vehicle::{Category, Reading, Telemetry, Update, Values};

impl Category {
    pub fn payload(self) -> Vec<u8> {
        proto::nested(
            2,
            &proto::nested(
                1,
                &proto::nested(
                    match self {
                        Self::Charge => 2,
                        Self::Tires => 14,
                    },
                    &[],
                ),
            ),
        )
    }
}
fn timestamp(m: Option<Message<'_>>) -> Result<Option<u64>> {
    let Some(m) = m else { return Ok(None) };
    let seconds = m.int(1)?.unwrap_or(0);
    let nanos = m.int(2)?.unwrap_or(0);
    if seconds > i64::MAX as u64 || nanos >= 1_000_000_000 {
        return Err(Error::Malformed);
    }
    Ok(Some(
        seconds
            .checked_mul(1000)
            .and_then(|s| s.checked_add(nanos / 1_000_000))
            .ok_or(Error::Malformed)?,
    ))
}
pub fn decode_state(category: Category, payload: &[u8], received_ms: u64) -> Result<Update> {
    let response = Message::parse(payload)?;
    if response
        .nested(1)?
        .and_then(|s| s.int(1).transpose())
        .transpose()?
        .unwrap_or(0)
        != 0
    {
        return Err(Error::Rejected);
    }
    let vehicle = response.nested(2)?.ok_or(Error::Malformed)?;
    let reading = |value: Option<f64>, sampled_ms, sampled_time_trusted| {
        value.map(|value| Reading {
            value,
            received_ms,
            sample_received_ms: received_ms,
            sampled_ms,
            sampled_time_trusted,
        })
    };
    match category {
        Category::Charge => {
            let state = vehicle.nested(3)?.ok_or(Error::Malformed)?;
            let at = timestamp(state.nested(44)?)?;
            let soc = state.int(114)?.filter(|v| *v <= 100).map(|v| v as f64);
            // Tesla BLE battery_range is rated miles; preserve this semantic, not estimated range.
            let range = state
                .fixed(111)?
                .map(f32::from_bits)
                .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 2000.0)
                .map(|v| f64::from(v) * 1.609344);
            Ok(Update::Charge {
                soc_pct: reading(soc, at, true),
                range_km: reading(range, at, true),
            })
        }
        Category::Tires => {
            let state = vehicle.nested(19)?.ok_or(Error::Malformed)?;
            let mut tires = [None; 4];
            for (n, tire) in tires.iter_mut().enumerate() {
                let value = state
                    .fixed(n as u32 + 2)?
                    .map(f32::from_bits)
                    .filter(|v| v.is_finite() && *v > 0.0 && *v <= 10.0)
                    .map(|v| f64::from(v) * 100.0);
                // Keep each sensor's original last-seen time, never relabel an old sample as new.
                // Tesla documents timezone errors in TPMS last-seen timestamps. Preserve raw
                // values and label their clock untrusted, without inventing a timezone fix.
                let sampled = timestamp(state.nested(n as u32 + 6)?)?;
                let clock_consistent =
                    sampled.is_some_and(|t| t <= received_ms && received_ms - t <= 120_000);
                *tire = reading(value, sampled, clock_consistent);
            }
            Ok(Update::Tires(tires))
        }
    }
}

#[cfg(test)]
mod tests;
