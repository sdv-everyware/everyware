//! The simulated room behind the thermostat: its temperature, its humidity and the
//! heating and cooling relays. Pure logic, no Matter: the cluster hooks in `main.rs` read
//! and drive it.
//!
//! Units are the Matter ones: temperatures in 0.01 °C, humidity in 0.01 %.

/// What the thermostat is set to do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Off,
    Heat,
    Cool,
    /// Heat below the heating setpoint, cool above the cooling one.
    Auto,
}

/// Which relay is closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Running {
    Idle,
    Heating,
    Cooling,
}

/// The room's temperature without heating or cooling, in 0.01 °C.
pub const AMBIENT: i16 = 1900;
/// How far the room moves per tick while heating or cooling, in 0.01 °C.
pub const ACTIVE_RATE: i16 = 20;
/// How far it moves back to [`AMBIENT`] per tick while idle, in 0.01 °C.
pub const IDLE_RATE: i16 = 5;
/// A relay closes once the room is this far past its setpoint, in 0.01 °C, and opens when
/// the setpoint is reached: without the band it would chatter around the setpoint.
pub const HYSTERESIS: i16 = 30;
/// The humidity stays within this range, in 0.01 %.
pub const HUMIDITY_MIN: u16 = 4000;
pub const HUMIDITY_MAX: u16 = 5500;

/// The room.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Room {
    pub temperature: i16,
    pub humidity: u16,
    pub mode: Mode,
    pub heating_setpoint: i16,
    pub cooling_setpoint: i16,
    pub running: Running,
    /// State of the humidity's random walk (xorshift).
    seed: u32,
}

impl Room {
    /// A room at `temperature` and `humidity`, the thermostat off.
    pub const fn new(temperature: i16, humidity: u16) -> Self {
        Self {
            temperature,
            humidity,
            mode: Mode::Off,
            heating_setpoint: 2200,
            cooling_setpoint: 2600,
            running: Running::Idle,
            seed: 0x2545_f491,
        }
    }

    /// The thermostat's new settings; the relays follow at once.
    pub fn apply(&mut self, mode: Mode, heating_setpoint: i16, cooling_setpoint: i16) {
        self.mode = mode;
        self.heating_setpoint = heating_setpoint;
        self.cooling_setpoint = cooling_setpoint;
        self.running = self.relay();
    }

    /// One tick of time: the temperature moves with the closed relay (or back to the ambient
    /// one), the humidity wanders, the relays follow. Returns whether the temperature, the
    /// humidity or the relays changed.
    pub fn tick(&mut self) -> Changes {
        let before = (self.temperature, self.humidity, self.running);
        self.temperature = match self.running {
            Running::Heating => self.temperature.saturating_add(ACTIVE_RATE),
            Running::Cooling => self.temperature.saturating_sub(ACTIVE_RATE),
            Running::Idle => toward(self.temperature, AMBIENT, IDLE_RATE),
        };
        self.humidity = self.next_humidity();
        self.running = self.relay();
        Changes {
            temperature: self.temperature != before.0,
            humidity: self.humidity != before.1,
            running: self.running != before.2,
        }
    }

    /// The relay the room asks for now, given the one that is closed.
    fn relay(&self) -> Running {
        let heat = matches!(self.mode, Mode::Heat | Mode::Auto);
        let cool = matches!(self.mode, Mode::Cool | Mode::Auto);
        match self.running {
            Running::Heating if heat && self.temperature < self.heating_setpoint => Running::Heating,
            Running::Cooling if cool && self.temperature > self.cooling_setpoint => Running::Cooling,
            _ if heat && self.temperature <= self.heating_setpoint - HYSTERESIS => Running::Heating,
            _ if cool && self.temperature >= self.cooling_setpoint + HYSTERESIS => Running::Cooling,
            _ => Running::Idle,
        }
    }

    /// A step of at most 0.2 % up or down, within the range; cooling dries the air a little.
    fn next_humidity(&mut self) -> u16 {
        let mut x = self.seed;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.seed = x;
        let step = (x % 41) as i32 - 20;
        let drying = if self.running == Running::Cooling { -10 } else { 0 };
        (self.humidity as i32 + step + drying).clamp(HUMIDITY_MIN as i32, HUMIDITY_MAX as i32) as u16
    }
}

/// What a tick changed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Changes {
    pub temperature: bool,
    pub humidity: bool,
    pub running: bool,
}

/// `value` moved by at most `rate` toward `target`.
fn toward(value: i16, target: i16, rate: i16) -> i16 {
    if value < target {
        (value + rate).min(target)
    } else {
        (value - rate).max(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heating_reaches_the_setpoint_and_stops() {
        let mut room = Room::new(2000, 4500);
        room.apply(Mode::Heat, 2200, 2600);
        assert_eq!(room.running, Running::Heating);
        for _ in 0..20 {
            room.tick();
        }
        assert!(room.temperature >= 2200, "{}", room.temperature);
        assert_eq!(room.running, Running::Idle);
    }

    #[test]
    fn the_band_keeps_the_relay_open_near_the_setpoint() {
        let mut room = Room::new(2190, 4500);
        room.apply(Mode::Heat, 2200, 2600);
        assert_eq!(room.running, Running::Idle, "10 below the setpoint is inside the band");
        room.temperature = 2160;
        room.tick();
        assert_eq!(room.running, Running::Heating);
    }

    #[test]
    fn auto_cools_above_the_cooling_setpoint() {
        let mut room = Room::new(2700, 4500);
        room.apply(Mode::Auto, 2200, 2600);
        assert_eq!(room.running, Running::Cooling);
        let before = room.temperature;
        room.tick();
        assert_eq!(room.temperature, before - ACTIVE_RATE);
    }

    #[test]
    fn off_drifts_to_ambient() {
        let mut room = Room::new(2300, 4500);
        room.apply(Mode::Off, 2200, 2600);
        for _ in 0..200 {
            room.tick();
        }
        assert_eq!((room.temperature, room.running), (AMBIENT, Running::Idle));
    }

    #[test]
    fn humidity_stays_in_range() {
        let mut room = Room::new(2200, HUMIDITY_MAX);
        for _ in 0..10_000 {
            room.tick();
            assert!((HUMIDITY_MIN..=HUMIDITY_MAX).contains(&room.humidity));
        }
    }
}
