//! Reading one node and sending it commands, over CASE on the house's fabric (rs-matter's
//! generated cluster clients: one transaction per attribute). An exchange reuses the node's
//! CASE session once there is one.
//!
//! The node's endpoints, their device types and clusters are read once; so is what does not
//! change (the limits, the mode sequence, what the fan supports, Basic Information): a poll
//! reads the rest. An attribute the cluster has as optional and the device does not serve is
//! left out; a mandatory one that fails fails the poll (the node did not answer).

use crate::model::*;
use rs_matter::crypto::Crypto;
use rs_matter::dm::clusters::decl::basic_information::BasicInformationClient;
use rs_matter::dm::clusters::decl::descriptor::DescriptorClient;
use rs_matter::dm::clusters::decl::fan_control::{FanControlClient, FanModeEnum};
use rs_matter::dm::clusters::decl::general_diagnostics::GeneralDiagnosticsClient;
use rs_matter::dm::clusters::decl::level_control::{LevelControlClient, OptionsBitmap};
use rs_matter::dm::clusters::decl::on_off::{OnOffClient, StartUpOnOffEnum};
use rs_matter::dm::clusters::decl::relative_humidity_measurement::RelativeHumidityMeasurementClient;
use rs_matter::dm::clusters::decl::thermostat::{SystemModeEnum, ThermostatClient};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::sc::ExchangeInitiator;
use rs_matter::tlv::Nullable;
use rs_matter::transport::exchange::Exchange;
use rs_matter::Matter;
use std::future::Future;
use std::num::NonZeroU8;

/// The clusters the house reads.
const ON_OFF: u32 = 0x0006;
const LEVEL_CONTROL: u32 = 0x0008;
const THERMOSTAT: u32 = 0x0201;
const FAN_CONTROL: u32 = 0x0202;
const RELATIVE_HUMIDITY: u32 = 0x0405;
/// Basic Information and General Diagnostics are on the root endpoint.
const ROOT: u16 = 0;

/// An endpoint as its Descriptor tells it.
#[derive(Clone, Debug, PartialEq)]
pub struct EndpointInfo {
    pub id: u16,
    pub device_type: u32,
    pub clusters: Vec<u32>,
}

/// One node of the fabric, reached by its node id.
pub struct Reader<'a, C: Crypto> {
    pub matter: &'static Matter<'static>,
    pub crypto: &'a C,
    pub fab_idx: NonZeroU8,
    pub node_id: u64,
}

/// An optional attribute: None when the device does not serve it.
async fn opt<T>(read: impl Future<Output = Result<T, Error>>) -> Option<T> {
    read.await.ok()
}

impl<C: Crypto> Reader<'_, C> {
    /// An exchange with the node (CASE: the existing session, else a new one, the node's
    /// address resolved over mDNS).
    async fn ex(&self) -> Result<Exchange<'static>, Error> {
        Exchange::initiate(self.matter, self.crypto, self.fab_idx, self.node_id).await
    }

    /// The node's endpoints (the root's PartsList), each with its device type and clusters.
    pub async fn structure(&self) -> Result<Vec<EndpointInfo>, Error> {
        let parts = self
            .ex()
            .await?
            .descriptor()
            .parts_list_read_with(ROOT, |r| -> Result<Vec<u16>, Error> { r?.iter().collect() })
            .await??;
        let mut endpoints = Vec::new();
        for id in parts {
            let device_type = self
                .ex()
                .await?
                .descriptor()
                .device_type_list_read_with(id, |r| -> Result<u32, Error> {
                    // the first one: an endpoint has its main device type first
                    r?.iter().next().ok_or(ErrorCode::NotFound)??.device_type()
                })
                .await??;
            let clusters = self.ex().await?.descriptor().server_list_read_with(id, |r| -> Result<Vec<u32>, Error> { r?.iter().collect() }).await??;
            endpoints.push(EndpointInfo { id, device_type, clusters });
        }
        Ok(endpoints)
    }

    /// The node's state now; what does not change comes from `prev` when it has it.
    pub async fn state(&self, endpoints: &[EndpointInfo], prev: &NodeState) -> Result<NodeState, Error> {
        let mut state = if prev.vendor.is_empty() { self.basic().await? } else { prev.clone() };
        state.node_id = self.node_id;
        state.uptime_s = self.ex().await?.general_diagnostics().up_time_read(ROOT).await?;
        state.endpoints = Vec::new();
        for info in endpoints {
            let before = prev.endpoints.iter().find(|e| e.id == info.id);
            let mut e = EndpointState { id: info.id, device_type: info.device_type, ..Default::default() };
            let has = |c: u32| info.clusters.contains(&c);
            if has(THERMOSTAT) {
                e.thermostat = Some(self.thermostat(info.id, before.and_then(|b| b.thermostat.as_ref())).await?);
            }
            if has(RELATIVE_HUMIDITY) {
                let v = self.ex().await?.relative_humidity_measurement().measured_value_read(info.id).await?;
                e.humidity = Some(Humidity { percent: v.into_option().map(|v| f32::from(v) / 100.0) });
            }
            if has(ON_OFF) {
                e.on_off = Some(self.on_off(info.id).await?);
            }
            if has(LEVEL_CONTROL) {
                e.level = Some(self.level(info.id, before.and_then(|b| b.level.as_ref())).await?);
            }
            if has(FAN_CONTROL) {
                e.fan = Some(self.fan(info.id, before.and_then(|b| b.fan.as_ref())).await?);
            }
            state.endpoints.push(e);
        }
        Ok(state)
    }

    /// Basic Information (once: it does not change while the node runs), and the boot reason.
    async fn basic(&self) -> Result<NodeState, Error> {
        let text = |r: Result<&str, Error>| r.map(str::to_string);
        Ok(NodeState {
            vendor: self.ex().await?.basic_information().vendor_name_read_with(ROOT, text).await??,
            product: self.ex().await?.basic_information().product_name_read_with(ROOT, text).await??,
            serial_number: opt(async { self.ex().await?.basic_information().serial_number_read_with(ROOT, text).await? }).await.unwrap_or_default(),
            software_version: self.ex().await?.basic_information().software_version_string_read_with(ROOT, text).await??,
            vendor_id: self.ex().await?.basic_information().vendor_id_read(ROOT).await?,
            product_id: self.ex().await?.basic_information().product_id_read(ROOT).await?,
            boot_reason: opt(async { self.ex().await?.general_diagnostics().boot_reason_read(ROOT).await }).await.map(|r| format!("{r:?}")).unwrap_or_default(),
            ..Default::default()
        })
    }

    async fn thermostat(&self, ep: u16, before: Option<&Thermostat>) -> Result<Thermostat, Error> {
        let mut t = match before {
            Some(b) => b.clone(),
            None => Thermostat {
                min_heat_setpoint_c: opt(async { self.ex().await?.thermostat().min_heat_setpoint_limit_read(ep).await }).await.map(centi_c).unwrap_or(7.0),
                max_heat_setpoint_c: opt(async { self.ex().await?.thermostat().max_heat_setpoint_limit_read(ep).await }).await.map(centi_c).unwrap_or(30.0),
                min_cool_setpoint_c: opt(async { self.ex().await?.thermostat().min_cool_setpoint_limit_read(ep).await }).await.map(centi_c).unwrap_or(16.0),
                max_cool_setpoint_c: opt(async { self.ex().await?.thermostat().max_cool_setpoint_limit_read(ep).await }).await.map(centi_c).unwrap_or(32.0),
                ..Default::default()
            },
        };
        t.local_temperature_c = self.ex().await?.thermostat().local_temperature_read(ep).await?.into_option().map(centi_c);
        t.system_mode = format!("{:?}", self.ex().await?.thermostat().system_mode_read(ep).await?);
        if let Some(v) = opt(async { self.ex().await?.thermostat().occupied_heating_setpoint_read(ep).await }).await {
            t.heating_setpoint_c = centi_c(v);
        }
        if let Some(v) = opt(async { self.ex().await?.thermostat().occupied_cooling_setpoint_read(ep).await }).await {
            t.cooling_setpoint_c = centi_c(v);
        }
        t.running = match opt(async { self.ex().await?.thermostat().thermostat_running_state_read(ep).await }).await {
            Some(bits) => match flags(u32::from(bits.bits()), &[(1, "heating"), (2, "cooling"), (4, "fan")]) {
                none if none.is_empty() => "idle".to_string(),
                names => names,
            },
            None => String::new(),
        };
        Ok(t)
    }

    async fn on_off(&self, ep: u16) -> Result<OnOff, Error> {
        let on = self.ex().await?.on_off().on_off_read(ep).await?;
        let start_up = match opt(async { self.ex().await?.on_off().start_up_on_off_read(ep).await }).await {
            Some(v) => match v.into_option() {
                Some(StartUpOnOffEnum::Off) => "Off",
                Some(StartUpOnOffEnum::On) => "On",
                Some(StartUpOnOffEnum::Toggle) => "Toggle",
                None => "Previous",
            },
            None => "",
        };
        Ok(OnOff { on, start_up: start_up.to_string() })
    }

    async fn level(&self, ep: u16, before: Option<&Level>) -> Result<Level, Error> {
        let mut l = match before {
            Some(b) => b.clone(),
            None => Level {
                min_level: opt(async { self.ex().await?.level_control().min_level_read(ep).await }).await.unwrap_or(1),
                max_level: opt(async { self.ex().await?.level_control().max_level_read(ep).await }).await.unwrap_or(254),
                ..Default::default()
            },
        };
        l.level = self.ex().await?.level_control().current_level_read(ep).await?.into_option();
        l.transition_ds = opt(async { self.ex().await?.level_control().on_off_transition_time_read(ep).await }).await.unwrap_or(0);
        l.start_up_level = opt(async { self.ex().await?.level_control().start_up_current_level_read(ep).await }).await.and_then(Nullable::into_option);
        Ok(l)
    }

    async fn fan(&self, ep: u16, before: Option<&Fan>) -> Result<Fan, Error> {
        let rock = [(1, "LeftRight"), (2, "UpDown"), (4, "Round")];
        let wind = [(1, "SleepWind"), (2, "NaturalWind")];
        let mut f = match before {
            Some(b) => b.clone(),
            None => Fan {
                mode_sequence: format!("{:?}", self.ex().await?.fan_control().fan_mode_sequence_read(ep).await?),
                rock_support: opt(async { self.ex().await?.fan_control().rock_support_read(ep).await }).await.map(|b| flags(u32::from(b.bits()), &rock)).unwrap_or_default(),
                wind_support: opt(async { self.ex().await?.fan_control().wind_support_read(ep).await }).await.map(|b| flags(u32::from(b.bits()), &wind)).unwrap_or_default(),
                ..Default::default()
            },
        };
        f.mode = format!("{:?}", self.ex().await?.fan_control().fan_mode_read(ep).await?);
        f.percent_setting = self.ex().await?.fan_control().percent_setting_read(ep).await?.into_option();
        f.percent_current = self.ex().await?.fan_control().percent_current_read(ep).await?;
        if !f.rock_support.is_empty() {
            f.rock_setting = opt(async { self.ex().await?.fan_control().rock_setting_read(ep).await }).await.map(|b| flags(u32::from(b.bits()), &rock)).unwrap_or_default();
        }
        if !f.wind_support.is_empty() {
            f.wind_setting = opt(async { self.ex().await?.fan_control().wind_setting_read(ep).await }).await.map(|b| flags(u32::from(b.bits()), &wind)).unwrap_or_default();
        }
        f.airflow_direction = opt(async { self.ex().await?.fan_control().airflow_direction_read(ep).await }).await.map(|d| format!("{d:?}")).unwrap_or_default();
        Ok(f)
    }

    /// Runs a device command (Pair is the controller's, not the node's).
    pub async fn command(&self, command: &Command) -> Result<(), Error> {
        match command {
            Command::SetOnOff { endpoint, on: true, .. } => self.ex().await?.on_off().on(*endpoint).await,
            Command::SetOnOff { endpoint, on: false, .. } => self.ex().await?.on_off().off(*endpoint).await,
            Command::MoveToLevel { endpoint, level, transition_ds, .. } => {
                let transition = match transition_ds {
                    Some(ds) => Nullable::some(*ds),
                    None => Nullable::none(),
                };
                self.ex()
                    .await?
                    .level_control()
                    .move_to_level(*endpoint, |req| req.level(*level)?.transition_time(transition.clone())?.options_mask(OptionsBitmap::empty())?.options_override(OptionsBitmap::empty())?.end())
                    .await
            }
            Command::SetSetpoint { endpoint, heating: true, celsius, .. } => self.ex().await?.thermostat().occupied_heating_setpoint_write(*endpoint, to_centi_c(*celsius)).await,
            Command::SetSetpoint { endpoint, heating: false, celsius, .. } => self.ex().await?.thermostat().occupied_cooling_setpoint_write(*endpoint, to_centi_c(*celsius)).await,
            Command::SetSystemMode { endpoint, mode, .. } => {
                let mode = match mode.as_str() {
                    "Off" => SystemModeEnum::Off,
                    "Auto" => SystemModeEnum::Auto,
                    "Cool" => SystemModeEnum::Cool,
                    "Heat" => SystemModeEnum::Heat,
                    _ => return Err(ErrorCode::InvalidData.into()),
                };
                self.ex().await?.thermostat().system_mode_write(*endpoint, mode).await
            }
            Command::SetFanMode { endpoint, mode, .. } => {
                let mode = match mode.as_str() {
                    "Off" => FanModeEnum::Off,
                    "Low" => FanModeEnum::Low,
                    "Medium" | "Med" => FanModeEnum::Medium,
                    "High" => FanModeEnum::High,
                    "On" => FanModeEnum::On,
                    "Auto" => FanModeEnum::Auto,
                    _ => return Err(ErrorCode::InvalidData.into()),
                };
                self.ex().await?.fan_control().fan_mode_write(*endpoint, mode).await
            }
            Command::Pair { .. } => Err(ErrorCode::InvalidAction.into()),
        }
    }
}

/// "ep1 0x0301 [0x0201 0x0405], ep2 ..." for the log.
pub fn describe(endpoints: &[EndpointInfo]) -> String {
    endpoints
        .iter()
        .map(|e| format!("ep{} type {:#06x} clusters [{}]", e.id, e.device_type, e.clusters.iter().map(|c| format!("{c:#06x}")).collect::<Vec<_>>().join(" ")))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The node's values in one line, for the log.
pub fn summary(n: &NodeState) -> String {
    let mut parts = vec![format!("{} {} sn {} sw {} up {} s", n.vendor, n.product, n.serial_number, n.software_version, n.uptime_s)];
    for e in &n.endpoints {
        let mut p = format!("ep{}", e.id);
        if let Some(t) = &e.thermostat {
            p += &format!(" thermostat {:?} °C heat {} cool {} {} {}", t.local_temperature_c, t.heating_setpoint_c, t.cooling_setpoint_c, t.system_mode, t.running);
        }
        if let Some(h) = &e.humidity {
            p += &format!(" humidity {:?} %", h.percent);
        }
        if let Some(o) = &e.on_off {
            p += &format!(" on {} (start-up {})", o.on, o.start_up);
        }
        if let Some(l) = &e.level {
            p += &format!(" level {:?} [{}..{}]", l.level, l.min_level, l.max_level);
        }
        if let Some(f) = &e.fan {
            p += &format!(" fan {} {:?}/{} % ({}) rock {} wind {}", f.mode, f.percent_setting, f.percent_current, f.mode_sequence, f.rock_setting, f.wind_setting);
        }
        parts.push(p);
    }
    parts.join("; ")
}
