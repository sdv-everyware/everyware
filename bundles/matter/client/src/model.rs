//! What the controller knows of its devices and what it can be asked to do: plain Rust, no
//! Matter and no SDV types (the bundle turns them into the catalog's messages).

/// Everything the controller knows, after a poll.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub nodes: Vec<NodeState>,
}

/// A commissioned node.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeState {
    pub node_id: u64,
    /// It answered the last poll.
    pub online: bool,
    /// Unix time of the last answer (0: never).
    pub last_seen: i64,
    pub vendor: String,
    pub product: String,
    pub serial_number: String,
    pub software_version: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub uptime_s: u64,
    pub boot_reason: String,
    pub endpoints: Vec<EndpointState>,
}

/// An endpoint: its device type and the clusters it serves that the house reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EndpointState {
    pub id: u16,
    pub device_type: u32,
    pub thermostat: Option<Thermostat>,
    pub humidity: Option<Humidity>,
    pub on_off: Option<OnOff>,
    pub level: Option<Level>,
    pub fan: Option<Fan>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Thermostat {
    /// The room's temperature, °C; None when the device has no reading.
    pub local_temperature_c: Option<f32>,
    pub heating_setpoint_c: f32,
    pub cooling_setpoint_c: f32,
    pub system_mode: String,
    /// "heating", "cooling", "fan", "idle" (from ThermostatRunningState, several joined by " + ").
    pub running: String,
    pub min_heat_setpoint_c: f32,
    pub max_heat_setpoint_c: f32,
    pub min_cool_setpoint_c: f32,
    pub max_cool_setpoint_c: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Humidity {
    pub percent: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OnOff {
    pub on: bool,
    /// "Off", "On", "Toggle", or "Previous" (the cluster's null).
    pub start_up: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Level {
    pub level: Option<u8>,
    pub min_level: u8,
    pub max_level: u8,
    /// Tenths of a second.
    pub transition_ds: u16,
    /// None: as before (the cluster's null).
    pub start_up_level: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fan {
    pub mode: String,
    pub mode_sequence: String,
    pub percent_setting: Option<u8>,
    pub percent_current: u8,
    pub airflow_direction: String,
    pub rock_support: String,
    pub rock_setting: String,
    pub wind_support: String,
    pub wind_setting: String,
}

/// What the controller can be asked to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Commission a device by its manual pairing code.
    Pair { code: String },
    SetOnOff { node_id: u64, endpoint: u16, on: bool },
    MoveToLevel { node_id: u64, endpoint: u16, level: u8, transition_ds: Option<u16> },
    SetSetpoint { node_id: u64, endpoint: u16, heating: bool, celsius: f32 },
    SetSystemMode { node_id: u64, endpoint: u16, mode: String },
    SetFanMode { node_id: u64, endpoint: u16, mode: String },
}

/// The answer to a command: the node id (Pair: the new one), or why not.
pub type Reply = Box<dyn FnOnce(Result<u64, String>) + Send>;

/// A temperature of the Thermostat cluster (hundredths of a degree) in °C.
pub fn centi_c(v: i16) -> f32 {
    f32::from(v) / 100.0
}

/// °C in the Thermostat cluster's hundredths, rounded.
pub fn to_centi_c(c: f32) -> i16 {
    (c * 100.0).round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

/// The set bits of a bitmap as names ("" when none is set).
pub fn flags(bits: u32, names: &[(u32, &str)]) -> String {
    names.iter().filter(|(bit, _)| bits & bit != 0).map(|(_, name)| *name).collect::<Vec<_>>().join(" + ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperatures_both_ways() {
        assert_eq!(centi_c(2150), 21.5);
        assert_eq!(to_centi_c(22.04), 2204);
        assert_eq!(to_centi_c(1e9), i16::MAX);
    }

    #[test]
    fn bitmap_names() {
        let names = [(1, "heating"), (2, "cooling"), (4, "fan")];
        assert_eq!(flags(0, &names), "");
        assert_eq!(flags(5, &names), "heating + fan");
    }
}
