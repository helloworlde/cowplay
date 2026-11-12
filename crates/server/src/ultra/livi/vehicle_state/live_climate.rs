use super::*;
use f50_core::vehicle::{ClimateCommand, ClimateState};

impl VehicleState {
    pub(super) fn with_live_climate(mut self) -> Self {
        let mut accessory = climate();
        accessory
            .services
            .retain(|s| [513, 518, 532].contains(&s.iid));
        for service in &mut accessory.services {
            service
                .characteristics
                .retain(|c| [HVAC_ON, 519, 520, 522, 533, 534, 536].contains(&c.iid));
            for c in &mut service.characteristics {
                if [HVAC_ON, 519, 520, 533, 534].contains(&c.iid) {
                    // Native controls need usable selections even before a vehicle
                    // connects. These are UI presets, never published to the Feed.
                    c.initial = Some(match c.iid {
                        HVAC_ON => Value::Bool(false),
                        519 | 533 => Value::Bool(true),
                        _ => Value::Real(21.0),
                    });
                }
                if [519, 533].contains(&c.iid) {
                    // Zone availability describes the UI, not Tesla HVAC power.
                    // Turning HVAC off or disconnecting must not disable the panel.
                    c.owner = Owner::Car;
                    c.mutable = false;
                }
            }
        }
        self.values
            .extend(initial_values(std::slice::from_ref(&accessory)));
        self.accessories.push(accessory);
        self
    }
    pub fn set_live_climate(&mut self, state: Option<&ClimateState>) -> Vec<(Lane, Vec<u8>)> {
        let fields = [
            (HVAC_ON, state.and_then(|s| s.hvac_on).map(Value::Bool)),
            (519, Some(Value::Bool(true))),
            (533, Some(Value::Bool(true))),
            (
                520,
                state
                    .and_then(|s| s.driver_temp_celsius)
                    .filter(|v| v.is_finite())
                    .map(|v| Value::Real(f64::from(v))),
            ),
            (
                534,
                state
                    .and_then(|s| s.passenger_temp_celsius)
                    .filter(|v| v.is_finite())
                    .map(|v| Value::Real(f64::from(v))),
            ),
        ];
        let range = state
            .and_then(|s| s.min_temp_celsius.zip(s.max_temp_celsius))
            .filter(|(a, b)| a.is_finite() && b.is_finite() && *a >= 0.0 && *b <= 50.0 && a < b);
        let mut changed = Vec::new();
        for (iid, value) in fields {
            // Retain the control's selection when telemetry expires. Actual climate
            // remains unknown in the Feed; live_climate_write still requires it.
            let value = value.or_else(|| self.values.get(&iid).cloned());
            for c in self
                .accessories
                .iter_mut()
                .flat_map(|a| &mut a.services)
                .flat_map(|s| &mut s.characteristics)
                .filter(|c| c.iid == iid)
            {
                c.initial = value.clone();
                if [520, 534].contains(&iid) {
                    if let Some((a, b)) = range {
                        c.range = Some((f64::from(a), f64::from(b), 0.5));
                    }
                    // The phone constructs a numeric control from this metadata even
                    // offline. Keep its template/last valid range when readings expire;
                    // it is not a measured temperature or permission to send a command.
                }
            }
            if let Some(value) = value {
                self.car_sets(iid, value, &mut changed);
            } else if self.values.remove(&iid).is_some() && self.followed.contains(&iid) {
                changed.push((Value::Int(iid), Value::Null));
            }
        }
        update_notify(changed).into_iter().collect()
    }
    pub fn live_climate_write(
        message: &[u8],
        current: Option<&ClimateState>,
    ) -> Option<(Option<i64>, Result<ClimateCommand, ()>)> {
        let request = live_request(message)?;
        if request.get("command").and_then(Value::as_str) != Some("writeRequest") {
            return None;
        }
        let written = request.get("values")?.as_dict()?;
        if !written
            .iter()
            .any(|(k, _)| k.as_int().is_some_and(|i| [HVAC_ON, 520, 534].contains(&i)))
        {
            return None;
        }
        let transaction = request.get("transactionID").and_then(Value::as_int);
        let command = (|| {
            if transaction.is_none()
                || message.len() > 2048
                || written.is_empty()
                || written.len() > 2
            {
                return Err(());
            }
            let current = current.ok_or(())?;
            if written.len() == 1 && written[0].0.as_int() == Some(HVAC_ON) {
                let Value::Bool(on) = written[0].1 else {
                    return Err(());
                };
                return Ok(ClimateCommand::Power { on });
            }
            let (min, max) = current
                .min_temp_celsius
                .zip(current.max_temp_celsius)
                .filter(|(a, b)| {
                    a.is_finite() && b.is_finite() && *a >= 0.0 && *b <= 50.0 && a <= b
                })
                .ok_or(())?;
            let (mut driver, mut passenger) =
                (current.driver_temp_celsius, current.passenger_temp_celsius);
            let mut seen = BTreeSet::new();
            for (key, value) in written {
                let iid = key.as_int().filter(|i| [520, 534].contains(i)).ok_or(())?;
                if !seen.insert(iid) {
                    return Err(());
                }
                let value = match value {
                    Value::Int(v) => *v as f64,
                    Value::Real(v) => *v,
                    _ => return Err(()),
                };
                if !value.is_finite() || value < f64::from(min) || value > f64::from(max) {
                    return Err(());
                }
                let value = value as f32;
                if iid == 520 {
                    driver = Some(value);
                } else {
                    passenger = Some(value);
                }
            }
            let (driver_celsius, passenger_celsius) = driver
                .zip(passenger)
                .filter(|(a, b)| {
                    [*a, *b]
                        .iter()
                        .all(|v| v.is_finite() && *v >= min && *v <= max)
                })
                .ok_or(())?;
            Ok(ClimateCommand::SetTemperature {
                driver_celsius,
                passenger_celsius,
            })
        })();
        Some((transaction, command))
    }
    pub fn live_climate_response(message: &[u8], success: bool) -> Vec<u8> {
        let request = live_request(message).unwrap();
        let results = request
            .get("values")
            .and_then(Value::as_dict)
            .unwrap_or(&[])
            .iter()
            .filter_map(|(k, _)| {
                Some((
                    Value::Int(k.as_int()?),
                    Value::Int(if success { WRITTEN } else { NO_VALUE }),
                ))
            })
            .collect();
        wrap(&dict([
            ("command", Value::str("writeResponse")),
            (
                "transactionID",
                request.get("transactionID").cloned().unwrap_or(Value::Null),
            ),
            ("errors", Value::Dict(results)),
        ]))
    }
}
fn live_request(message: &[u8]) -> Option<Value> {
    let outer = opack::decode(message)?;
    if outer.get("pluginID").and_then(Value::as_int) != Some(PLUGIN_ID) {
        return None;
    }
    opack::decode(outer.get("pluginData")?.as_data()?)
}
