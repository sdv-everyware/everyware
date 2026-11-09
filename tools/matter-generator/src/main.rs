//! matter-generator: a virtual Matter device for SDV EveryWare. One node with the four
//! devices of the dashboard, so the panel's Matter bundle has something real to commission,
//! read and drive:
//!
//! | Endpoint | Device type | Clusters (besides Descriptor, Identify) | Dashboard tile |
//! |---|---|---|---|
//! | 1 | Thermostat | Thermostat (heat, cool, auto), Relative Humidity Measurement | Home climate |
//! | 2 | On/Off Light | Groups, On/Off | Entryway |
//! | 3 | Dimmable Light | Groups, On/Off, Level Control | Bedroom |
//! | 4 | Fan | Groups, Fan Control (Low/Med/High, rocking, wind, airflow direction) | Office |
//!
//! Endpoint 0 carries Basic Information, General Diagnostics and the rest of the root
//! clusters rs-matter serves. The room behind the thermostat is simulated (`sim.rs`).

mod sim;

use core::cell::Cell;
use core::pin::pin;
use std::path::PathBuf;

use async_signal::{Signal, Signals};
use futures_lite::StreamExt;
use log::info;

use rs_matter::crypto::{default_crypto, Crypto};
use rs_matter::dm::clusters::app::fan_control::{
    self, AirflowDirectionEnum, CurrentSpeed, FanControlHooks, FanModeSequenceEnum, FanSetting, RockBitmap,
    WindBitmap,
};
use rs_matter::dm::clusters::app::level_control::{self, LevelControlHooks};
use rs_matter::dm::clusters::app::on_off::{self, NoLevelControl, OnOffHooks};
use rs_matter::dm::clusters::app::thermostat::{
    self, ControlSequenceOfOperationEnum, OutOfBandMessage, RelayStateBitmap, SystemModeEnum, ThermostatHooks,
};
use rs_matter::dm::clusters::decl::level_control as level_cluster;
use rs_matter::dm::clusters::decl::on_off as on_off_cluster;
use rs_matter::dm::clusters::decl::relative_humidity_measurement as humidity_cluster;
use rs_matter::dm::clusters::decl::thermostat as thermostat_cluster;
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::clusters::groups::{self, ClusterHandler as _};
use rs_matter::dm::clusters::identify::{self, IdentifyHandler};
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_DET};
use rs_matter::dm::devices::{DEV_TYPE_DIMMABLE_LIGHT, DEV_TYPE_FAN, DEV_TYPE_ON_OFF_LIGHT, DEV_TYPE_THERMOSTAT};
use rs_matter::dm::endpoints::ROOT_ENDPOINT_ID;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::{Async, Cluster, Dataver, EmptyHandler, Endpoint, HandlerContext, Node, ReadContext};
use rs_matter::error::Error;
use rs_matter::stack::eth::EthMatterStack;
use rs_matter::stack::net::std::StdNetStack;
use rs_matter::tlv::Nullable;
use rs_matter::transport::network::mdns::builtin::BuiltinMdns;
use rs_matter::utils::cell::RefCell;
use rs_matter::utils::futures::{select, Coalesce};
use rs_matter::utils::init::InitMaybeUninit;
use rs_matter::utils::sync::blocking::Mutex;
use rs_matter::utils::timers::{Duration, Timer};
use rs_matter::basic_info::BasicInfoConfig;
use rs_matter::{clusters, devices, root_endpoint, with, BasicCommData};

use static_cell::StaticCell;

use sim::{Mode, Room, Running};

const THERMOSTAT: u16 = 1;
const ENTRYWAY: u16 = 2;
const BEDROOM: u16 = 3;
const OFFICE: u16 = 4;

/// The bump allocator of the stack, in bytes; an unoptimized build needs more.
const BUMP_SIZE: usize = 48 * 1024;
/// How often the simulated room moves.
const TICK: Duration = Duration::from_secs(5);

/// The node's Basic Information. Vendor and product IDs stay the test ones: the device
/// attestation credentials (the test DAC) are issued for them, and controllers accept them
/// only as a test device.
const DEV_DET: BasicInfoConfig<'static> = BasicInfoConfig {
    vendor_name: "EveryWare",
    product_name: "Virtual Home",
    device_name: "Virtual Home",
    serial_no: "EW-VH-0001",
    hw_ver: 1,
    hw_ver_str: "1",
    sw_ver: 1,
    sw_ver_str: "0.1.0",
    ..TEST_DEV_DET
};

/// The Matter stack, statically allocated: its futures are large.
static MATTER_STACK: StaticCell<EthMatterStack<BUMP_SIZE, ()>> = StaticCell::new();

/// The command line.
struct Args {
    discriminator: u16,
    passcode: u32,
    port: u16,
    storage: PathBuf,
}

impl Args {
    /// The defaults are the ones chip-tool's tests use.
    fn parse() -> Result<Self, String> {
        let mut args = Args {
            discriminator: 3840,
            passcode: 20202021,
            port: 5540,
            storage: std::env::temp_dir().join("matter-generator"),
        };
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            let mut value = || it.next().ok_or(format!("{flag} needs a value"));
            match flag.as_str() {
                "--discriminator" => args.discriminator = value()?.parse().map_err(|e| format!("{flag}: {e}"))?,
                "--passcode" => args.passcode = value()?.parse().map_err(|e| format!("{flag}: {e}"))?,
                "--port" => args.port = value()?.parse().map_err(|e| format!("{flag}: {e}"))?,
                "--storage" => args.storage = PathBuf::from(value()?),
                "-h" | "--help" => {
                    return Err("usage: matter-generator [--discriminator N] [--passcode N] [--port N] [--storage DIR]".into())
                }
                _ => return Err(format!("unknown argument {flag}")),
            }
        }
        if args.discriminator > 0xfff {
            return Err("--discriminator is 12 bits (0..=4095)".into());
        }
        Ok(args)
    }
}

fn main() -> Result<(), Error> {
    env_logger::init_from_env(env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"));
    let args = match Args::parse() {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    let comm = BasicCommData {
        password: rs_matter::pairing::Spake2pVerifierPassword::new_from_ref(
            rs_matter::pairing::Spake2pVerifierPasswordRef::new(&args.passcode.to_le_bytes()),
        ),
        discriminator: args.discriminator,
    };
    let stack = MATTER_STACK
        .uninit()
        .init_with(EthMatterStack::init_with_port(&DEV_DET, comm, &TEST_DEV_ATT, args.port));

    // The fabrics, the ACLs and the clusters' settings survive a restart: once
    // commissioned, the node stays in the panel's fabric.
    std::fs::create_dir_all(&args.storage).map_err(|_| rs_matter::error::ErrorCode::StdIoError)?;
    info!("state in {}", args.storage.display());
    let mut store = rs_matter::persist::DirKvBlobStore::new(args.storage.clone());

    let crypto = default_crypto(rand::rng(), DAC_PRIVKEY);
    let mut rand = crypto.rand()?;

    // Prints the QR code and the pairing code while the node is not commissioned.
    futures_lite::future::block_on(stack.startup(&crypto, &mut store))?;
    let kv = stack.kv(store);

    let room = Mutex::new(RefCell::new(Room::new(2050, 4500)));

    let thermostat_handler = thermostat::ThermostatHandler::new(
        Dataver::new_rand(&mut rand),
        THERMOSTAT,
        rs_matter::persist::VENDOR_KEYS_START + 0x10,
        ThermostatLogic { room: &room },
    );
    let humidity_handler = HumidityHandler { dataver: Dataver::new_rand(&mut rand), room: &room };

    let entryway_handler = on_off::OnOffHandler::<_, NoLevelControl>::new(
        Dataver::new_rand(&mut rand),
        ENTRYWAY,
        rs_matter::persist::VENDOR_KEYS_START + 0x20,
        LightLogic { name: "entryway" },
    );
    entryway_handler.init(None);

    let bedroom_on_off = on_off::OnOffHandler::new(
        Dataver::new_rand(&mut rand),
        BEDROOM,
        rs_matter::persist::VENDOR_KEYS_START + 0x30,
        LightLogic { name: "bedroom" },
    );
    let bedroom_level = level_control::LevelControlHandler::new(
        Dataver::new_rand(&mut rand),
        BEDROOM,
        rs_matter::persist::VENDOR_KEYS_START + 0x31,
        DimmerLogic,
    );
    bedroom_on_off.init(Some(&bedroom_level));
    bedroom_level.init(Some(&bedroom_on_off));

    let fan_handler = fan_control::FanControlHandler::new(
        Dataver::new_rand(&mut rand),
        OFFICE,
        rs_matter::persist::VENDOR_KEYS_START + 0x40,
        FanLogic { percent: Mutex::new(Cell::new(0)) },
    );

    let mut term_signal = Signals::new([Signal::Term, Signal::Int])?;
    let mut term = pin!(async {
        term_signal.next().await;
        Ok(())
    });

    let mut matter = pin!(stack.run_preex(
        StdNetStack::new(),
        SysNetifs,
        // no Ethernet diagnostics
        &(),
        BuiltinMdns::new(),
        &crypto,
        (
            NODE,
            EmptyHandler
                .chain(
                    |e, _| e == ROOT_ENDPOINT_ID,
                    Async(EthMatterStack::<0, ()>::root_handler(stack.matter(), &(), rand)),
                )
                .chain(|e, c| e != ROOT_ENDPOINT_ID && c == desc::DescHandler::CLUSTER.id, Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()))
                .chain(|e, c| e != ROOT_ENDPOINT_ID && c == identify::CLUSTER.id, Async(IdentifyHandler::new(Dataver::new_rand(&mut rand)).adapt()))
                .chain(
                    |e, c| e != ROOT_ENDPOINT_ID && e != THERMOSTAT && c == groups::GroupsHandler::CLUSTER.id,
                    Async(groups::GroupsHandler::new(Dataver::new_rand(&mut rand), stack.matter()).adapt()),
                )
                .chain(|e, c| e == THERMOSTAT && c == ThermostatLogic::CLUSTER.id, Async(thermostat::HandlerAdaptor(&thermostat_handler)))
                .chain(|e, c| e == THERMOSTAT && c == HUMIDITY_CLUSTER.id, Async(humidity_cluster::HandlerAdaptor(&humidity_handler)))
                .chain(|e, c| e == ENTRYWAY && c == LightLogic::CLUSTER.id, Async(on_off::HandlerAdaptor(&entryway_handler)))
                .chain(|e, c| e == BEDROOM && c == LightLogic::CLUSTER.id, Async(on_off::HandlerAdaptor(&bedroom_on_off)))
                .chain(|e, c| e == BEDROOM && c == DimmerLogic::CLUSTER.id, Async(level_control::HandlerAdaptor(&bedroom_level)))
                .chain(|e, c| e == OFFICE && c == FanLogic::CLUSTER.id, Async(fan_control::HandlerAdaptor(&fan_handler))),
        ),
        kv,
        (),
    ));

    futures_lite::future::block_on(select(&mut matter, &mut term).coalesce())
}

/// The node: the root endpoint and the four devices.
const NODE: Node<'static> = Node {
    endpoints: &[
        root_endpoint!(eth),
        Endpoint::new(
            THERMOSTAT,
            devices!(DEV_TYPE_THERMOSTAT),
            clusters!(desc::DescHandler::CLUSTER, identify::CLUSTER, ThermostatLogic::CLUSTER, HUMIDITY_CLUSTER),
        ),
        Endpoint::new(
            ENTRYWAY,
            devices!(DEV_TYPE_ON_OFF_LIGHT),
            clusters!(desc::DescHandler::CLUSTER, identify::CLUSTER, groups::GroupsHandler::CLUSTER, LightLogic::CLUSTER),
        ),
        Endpoint::new(
            BEDROOM,
            devices!(DEV_TYPE_DIMMABLE_LIGHT),
            clusters!(
                desc::DescHandler::CLUSTER,
                identify::CLUSTER,
                groups::GroupsHandler::CLUSTER,
                LightLogic::CLUSTER,
                DimmerLogic::CLUSTER
            ),
        ),
        Endpoint::new(
            OFFICE,
            devices!(DEV_TYPE_FAN),
            clusters!(desc::DescHandler::CLUSTER, identify::CLUSTER, groups::GroupsHandler::CLUSTER, FanLogic::CLUSTER),
        ),
    ],
};

// Endpoint 1: the thermostat, on the simulated room

/// The thermostat's device side: it reads the room and closes its relays.
struct ThermostatLogic<'a> {
    room: &'a Mutex<RefCell<Room>>,
}

impl ThermostatHooks for ThermostatLogic<'_> {
    const CLUSTER: Cluster<'static> = thermostat_cluster::FULL_CLUSTER
        .with_revision(11)
        .with_features(
            thermostat_cluster::Feature::HEATING.bits()
                | thermostat_cluster::Feature::COOLING.bits()
                | thermostat_cluster::Feature::AUTO_MODE.bits(),
        )
        .with_attrs(with!(
            required;
            thermostat_cluster::AttributeId::AbsMinHeatSetpointLimit
                | thermostat_cluster::AttributeId::AbsMaxHeatSetpointLimit
                | thermostat_cluster::AttributeId::AbsMinCoolSetpointLimit
                | thermostat_cluster::AttributeId::AbsMaxCoolSetpointLimit
                | thermostat_cluster::AttributeId::OccupiedHeatingSetpoint
                | thermostat_cluster::AttributeId::OccupiedCoolingSetpoint
                | thermostat_cluster::AttributeId::MinHeatSetpointLimit
                | thermostat_cluster::AttributeId::MaxHeatSetpointLimit
                | thermostat_cluster::AttributeId::MinCoolSetpointLimit
                | thermostat_cluster::AttributeId::MaxCoolSetpointLimit
                | thermostat_cluster::AttributeId::MinSetpointDeadBand
                | thermostat_cluster::AttributeId::ThermostatRunningMode
                | thermostat_cluster::AttributeId::ThermostatRunningState
        ))
        .with_cmds(with!(thermostat_cluster::CommandId::SetpointRaiseLower))
        .with_events(with!());

    const CONTROL_SEQUENCE_OF_OPERATION: ControlSequenceOfOperationEnum =
        ControlSequenceOfOperationEnum::CoolingAndHeating;
    const OCCUPIED_HEATING_SETPOINT: i16 = 2200;
    const OCCUPIED_COOLING_SETPOINT: i16 = 2600;
    const SYSTEM_MODE: SystemModeEnum = SystemModeEnum::Heat;

    fn local_temperature(&self) -> Option<i16> {
        Some(self.room.lock(|room| room.borrow().temperature))
    }

    fn running_state(&self) -> RelayStateBitmap {
        match self.room.lock(|room| room.borrow().running) {
            Running::Heating => RelayStateBitmap::HEAT,
            Running::Cooling => RelayStateBitmap::COOL,
            Running::Idle => RelayStateBitmap::empty(),
        }
    }

    fn apply(&self, system_mode: SystemModeEnum, heating_setpoint: i16, cooling_setpoint: i16) {
        let mode = match system_mode {
            SystemModeEnum::Heat => Mode::Heat,
            SystemModeEnum::Cool => Mode::Cool,
            SystemModeEnum::Auto => Mode::Auto,
            _ => Mode::Off,
        };
        let (temperature, running) = self.room.lock(|room| {
            let mut room = room.borrow_mut();
            room.apply(mode, heating_setpoint, cooling_setpoint);
            (room.temperature, room.running)
        });
        info!(
            "thermostat: {system_mode:?}, heat {}, cool {}, room {} ({running:?})",
            celsius(heating_setpoint),
            celsius(cooling_setpoint),
            celsius(temperature)
        );
    }

    async fn run<F: Fn(OutOfBandMessage)>(&self, notify: F) {
        loop {
            Timer::after(TICK).await;
            let changes = self.room.lock(|room| room.borrow_mut().tick());
            match (changes.temperature, changes.running) {
                (true, true) => notify(OutOfBandMessage::Update),
                (true, false) => notify(OutOfBandMessage::LocalTemperature),
                (false, true) => notify(OutOfBandMessage::RunningState),
                (false, false) => {}
            }
        }
    }
}

/// 0.01 °C as text.
fn celsius(value: i16) -> String {
    format!("{}.{:02} °C", value / 100, (value % 100).abs())
}

/// Relative Humidity Measurement on the thermostat's endpoint: the room's humidity.
const HUMIDITY_CLUSTER: Cluster<'static> = humidity_cluster::FULL_CLUSTER.with_attrs(with!(required)).with_cmds(with!());

/// rs-matter has no handler for the cluster: this one serves the generated trait.
struct HumidityHandler<'a> {
    dataver: Dataver,
    room: &'a Mutex<RefCell<Room>>,
}

impl humidity_cluster::ClusterHandler for HumidityHandler<'_> {
    const CLUSTER: Cluster<'static> = HUMIDITY_CLUSTER;

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    /// The thermostat's task moves the humidity; this one reports it.
    async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
        let mut reported = self.humidity();
        loop {
            Timer::after(TICK).await;
            let now = self.humidity();
            if now != reported {
                reported = now;
                self.dataver_changed();
                ctx.notify_attr_changed(THERMOSTAT, HUMIDITY_CLUSTER.id, humidity_cluster::AttributeId::MeasuredValue as _);
            }
        }
    }

    fn measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(self.humidity()))
    }

    fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(sim::HUMIDITY_MIN))
    }

    fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(sim::HUMIDITY_MAX))
    }
}

impl HumidityHandler<'_> {
    fn humidity(&self) -> u16 {
        self.room.lock(|room| room.borrow().humidity)
    }
}

// Endpoints 2 and 3: the lights

/// A light: the handler owns and persists On/Off, the light only logs what it is told.
struct LightLogic {
    name: &'static str,
}

impl OnOffHooks for LightLogic {
    const CLUSTER: Cluster<'static> = on_off_cluster::FULL_CLUSTER
        .with_revision(6)
        .with_features(on_off_cluster::Feature::LIGHTING.bits())
        .with_attrs(with!(
            required;
            on_off_cluster::AttributeId::OnOff
                | on_off_cluster::AttributeId::GlobalSceneControl
                | on_off_cluster::AttributeId::OnTime
                | on_off_cluster::AttributeId::OffWaitTime
                | on_off_cluster::AttributeId::StartUpOnOff
        ))
        .with_cmds(with!(
            on_off_cluster::CommandId::Off
                | on_off_cluster::CommandId::On
                | on_off_cluster::CommandId::Toggle
                | on_off_cluster::CommandId::OffWithEffect
                | on_off_cluster::CommandId::OnWithRecallGlobalScene
                | on_off_cluster::CommandId::OnWithTimedOff
        ));

    /// On, as on the dashboard. `StartUpOnOff` stays null: after a power loss the light
    /// comes back as it was.
    const ON_OFF: bool = true;

    fn set_on_off(&self, on: bool) {
        info!("{}: light {}", self.name, if on { "on" } else { "off" });
    }

    async fn handle_off_with_effect(&self, _effect: on_off::EffectVariantEnum) {}
}

/// The bedroom's dimmer; the handler owns and persists the level.
struct DimmerLogic;

impl LevelControlHooks for DimmerLogic {
    const MIN_LEVEL: u8 = 1;
    const MAX_LEVEL: u8 = 254;
    const FASTEST_RATE: u8 = 50;
    /// 65 %, as on the dashboard.
    const CURRENT_LEVEL: Option<u8> = Some(166);
    /// Half a second between off and on (0.1 s units).
    const ON_OFF_TRANSITION_TIME: u16 = 5;
    const CLUSTER: Cluster<'static> = level_cluster::FULL_CLUSTER
        .with_features(level_control::Feature::LIGHTING.bits() | level_control::Feature::ON_OFF.bits())
        .with_attrs(with!(
            required;
            level_cluster::AttributeId::CurrentLevel
                | level_cluster::AttributeId::RemainingTime
                | level_cluster::AttributeId::MinLevel
                | level_cluster::AttributeId::MaxLevel
                | level_cluster::AttributeId::OnOffTransitionTime
                | level_cluster::AttributeId::OnLevel
                | level_cluster::AttributeId::OnTransitionTime
                | level_cluster::AttributeId::OffTransitionTime
                | level_cluster::AttributeId::DefaultMoveRate
                | level_cluster::AttributeId::Options
                | level_cluster::AttributeId::StartUpCurrentLevel
        ))
        .with_cmds(with!(
            level_cluster::CommandId::MoveToLevel
                | level_cluster::CommandId::Move
                | level_cluster::CommandId::Step
                | level_cluster::CommandId::Stop
                | level_cluster::CommandId::MoveToLevelWithOnOff
                | level_cluster::CommandId::MoveWithOnOff
                | level_cluster::CommandId::StepWithOnOff
                | level_cluster::CommandId::StopWithOnOff
        ));

    fn set_device_level(&self, level: u8) -> Result<Option<u8>, ()> {
        info!("bedroom: level {level} ({} %)", (level as u32 * 100 + 127) / 254);
        Ok(Some(level))
    }
}

// Endpoint 4: the office fan

/// The office fan: reaches the speed it is asked for at once.
struct FanLogic {
    /// What it runs at, 0..=100.
    percent: Mutex<Cell<u8>>,
}

impl FanControlHooks for FanLogic {
    const CLUSTER: Cluster<'static> = fan_control::FULL_CLUSTER
        .with_revision(6)
        .with_features(
            fan_control::Feature::ROCKING.bits()
                | fan_control::Feature::WIND.bits()
                | fan_control::Feature::AIRFLOW_DIRECTION.bits(),
        )
        .with_attrs(with!(
            required;
            fan_control::AttributeId::RockSupport
                | fan_control::AttributeId::RockSetting
                | fan_control::AttributeId::WindSupport
                | fan_control::AttributeId::WindSetting
                | fan_control::AttributeId::AirflowDirection
        ))
        .with_cmds(with!());

    const FAN_MODE_SEQUENCE: FanModeSequenceEnum = FanModeSequenceEnum::OffLowMedHigh;
    const ROCK_SUPPORT: RockBitmap = RockBitmap::ROCK_LEFT_RIGHT;
    const WIND_SUPPORT: WindBitmap = WindBitmap::NATURAL_WIND;
    /// Low, as on the dashboard.
    const PERCENT_SETTING: Option<u8> = Some(33);
    const AIRFLOW_DIRECTION: AirflowDirectionEnum = AirflowDirectionEnum::Forward;

    fn set_fan(&self, setting: FanSetting) -> Result<(), ()> {
        let percent = match setting {
            FanSetting::Off | FanSetting::Auto => 0,
            FanSetting::Manual { percent, .. } => percent,
        };
        info!("office: fan {setting:?}");
        self.percent.lock(|p| p.set(percent));
        Ok(())
    }

    fn current_speed(&self) -> CurrentSpeed {
        CurrentSpeed::Percent(self.percent.lock(|p| p.get()))
    }

    fn set_rock_setting(&self, setting: RockBitmap) -> RockBitmap {
        info!("office: rocking {setting:?}");
        setting
    }

    fn set_wind_setting(&self, setting: WindBitmap) -> WindBitmap {
        info!("office: wind {setting:?}");
        setting
    }

    fn set_airflow_direction(&self, direction: AirflowDirectionEnum) {
        info!("office: airflow {direction:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn celsius_as_text() {
        assert_eq!(celsius(2205), "22.05 °C");
        assert_eq!(celsius(-150), "-1.50 °C");
    }
}
