use super::{
    proto::{self, Message},
    timestamp, Error, Result,
};

pub use crate::vehicle::{ClimateCommand, ClimateState};

impl ClimateCommand {
    pub(super) fn payload(self) -> Result<Vec<u8>> {
        let (field, value) = match self {
            Self::Power { on } => {
                let mut value = Vec::new();
                if on {
                    proto::int(1, 1, &mut value);
                }
                (10, value)
            }
            Self::SetTemperature {
                driver_celsius,
                passenger_celsius,
            } => {
                if [driver_celsius, passenger_celsius]
                    .iter()
                    .any(|t| !t.is_finite() || !(0.0..=50.0).contains(t))
                {
                    return Err(Error::Malformed);
                }
                let mut value = proto::nested(5, &proto::nested(3, &[]));
                proto::fixed(6, driver_celsius.to_bits(), &mut value);
                proto::fixed(7, passenger_celsius.to_bits(), &mut value);
                (14, value)
            }
        };
        Ok(proto::nested(2, &proto::nested(field, &value)))
    }
}

pub(super) fn query() -> Vec<u8> {
    proto::nested(2, &proto::nested(1, &proto::nested(3, &[])))
}

fn response(payload: &[u8]) -> Result<Message<'_>> {
    let response = Message::parse(payload)?;
    if response
        .nested(1)?
        .ok_or(Error::Malformed)?
        .int(1)?
        .unwrap_or(0)
        != 0
    {
        return Err(Error::Rejected);
    }
    Ok(response)
}

pub(super) fn decode(payload: &[u8]) -> Result<ClimateState> {
    let state = response(payload)?
        .nested(2)?
        .ok_or(Error::Malformed)?
        .nested(4)?
        .ok_or(Error::Malformed)?;
    let temperature = |field| -> Result<Option<f32>> {
        Ok(state
            .fixed(field)?
            .map(f32::from_bits)
            .filter(|t| t.is_finite()))
    };
    Ok(ClimateState {
        hvac_on: state.int(110)?.map(|on| on != 0),
        inside_temp_celsius: temperature(101)?,
        outside_temp_celsius: temperature(102)?,
        driver_temp_celsius: temperature(103)?,
        passenger_temp_celsius: temperature(104)?,
        min_temp_celsius: temperature(111)?,
        max_temp_celsius: temperature(112)?,
        sampled_ms: timestamp(state.nested(33)?)?,
    })
}

pub(super) fn ack(payload: &[u8]) -> Result<()> {
    response(payload)?;
    Ok(())
}
