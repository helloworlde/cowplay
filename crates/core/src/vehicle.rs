//! Vehicle readings shared by protocol adapters and presentation transports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClimateCommand {
    Power { on: bool },
    SetTemperature { driver_celsius: f32, passenger_celsius: f32 },
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct ClimateState {
    pub hvac_on: Option<bool>,
    pub inside_temp_celsius: Option<f32>,
    pub outside_temp_celsius: Option<f32>,
    pub driver_temp_celsius: Option<f32>,
    pub passenger_temp_celsius: Option<f32>,
    pub min_temp_celsius: Option<f32>,
    pub max_temp_celsius: Option<f32>,
    pub sampled_ms: Option<u64>,
}
impl ClimateState {
    pub fn fresh(&self, received_ms: u64, now: u64) -> bool {
        now.saturating_sub(received_ms) <= 30_000 && self.sampled_ms.is_some_and(|t| t <= now.saturating_add(5000) && now.saturating_sub(t) <= 30_000)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Charge,
    Tires,
}
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Reading {
    pub value: f64,
    pub received_ms: u64,
    pub sample_received_ms: u64,
    pub sampled_ms: Option<u64>,
    pub sampled_time_trusted: bool,
}
impl Reading {
    pub fn age_ms(&self, now: u64) -> u64 {
        now.saturating_sub(self.received_ms)
            .max(if self.sampled_time_trusted {
                self.sampled_ms.map_or(0, |t| now.saturating_sub(t))
            } else {
                now.saturating_sub(self.sample_received_ms)
            })
    }
    pub fn fresh(&self, now: u64, ttl: u64) -> bool {
        self.sampled_time_trusted
            && self.age_ms(now) <= ttl
            && self
                .sampled_ms
                .is_none_or(|t| t <= now.saturating_add(5000))
    }
}
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Telemetry {
    pub soc_pct: Option<Reading>,
    pub range_km: Option<Reading>,
    pub tires_kpa: [Option<Reading>; 4],
}
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Values {
    pub soc_pct: Option<f64>,
    pub range_km: Option<f64>,
    pub tires_kpa: [Option<f64>; 4],
}
impl Telemetry {
    pub fn fresh(&self, now: u64) -> Values {
        let value = |r: Option<Reading>, ttl| r.filter(|r| r.fresh(now, ttl)).map(|r| r.value);
        Values {
            soc_pct: value(self.soc_pct, 30_000),
            range_km: value(self.range_km, 30_000),
            tires_kpa: self.tires_kpa.map(|r| value(r, 120_000)),
        }
    }
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Charge { soc_pct, range_km } => {
                self.soc_pct = soc_pct;
                self.range_km = range_km;
            }
            Update::Tires(mut tires) => {
                for (new, old) in tires.iter_mut().zip(self.tires_kpa) {
                    if let (Some(new), Some(old)) = (new, old) {
                        if new.value == old.value && new.sampled_ms == old.sampled_ms {
                            new.sample_received_ms = old.sample_received_ms;
                        }
                    }
                }
                self.tires_kpa = tires;
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Charge {
        soc_pct: Option<Reading>,
        range_km: Option<Reading>,
    },
    Tires([Option<Reading>; 4]),
}
