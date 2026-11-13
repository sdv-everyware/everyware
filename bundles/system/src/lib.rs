//! SystemServiceBundle: owns the device's system settings in SDV's user_preferences and applies
//! them. The group `time` holds the time zone (an IANA name, applied to the system through
//! `persist.sys.timezone`, which bionic reads) and whether the clock shows 24 hours; the group
//! `units` holds the unit of temperatures, Celsius or Fahrenheit. The clock's format and the
//! unit are applied by the screens that show a clock or a temperature.
//!
//! The shape is SDV's user-controllable service: at start the bundle registers the group with
//! its defaults (UserPreferencesRegistryService) and reports the values in force
//! (UpdateSettings); a screen asks user_preferences for a change
//! (UserPreferencesManagementService), user_preferences asks the bundle
//! (UserControllableService::RequestSettingsChange), the bundle applies it and answers with the
//! value in force, which user_preferences keeps and sends to the screens.
//!
//! The settings are SHARED (the device's, not a user's). user_preferences pushes no SHARED value
//! to its owner at start and keeps whatever the owner reports, so the bundle is their source of
//! truth at boot: the time zone survives a reboot in its property, the clock's format and the
//! unit in the bundle's own directory.

use async_trait::async_trait;
use com_android_sdv_user_preferences_user_controllable_user_controllable_service_rpc::Interface as UserControllable;
use com_android_sdv_user_preferences_user_preferences_registry_service_rpc::Client as RegistryClient;
use everyware_bundle_store::Store;
use futures_util::stream::StreamExt;
use log::{debug, error, info, warn};
use protobuf::{EnumOrUnknown, MessageField};
use user_preferences_api::setting::setting_with_constraints::Constraints;
use user_preferences_api::setting::EnumConstraints;
use sdv::mw::{clientlib, Availability, Communicate, SdvComms};
use sdv::status::{SdvResult, SdvStatus, SdvStatusCode};
use sdv_mw_rs_com_everyware_system_service_bundle::{ClientDescriptors, SystemServiceBundle as Bundle};
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use user_preferences_api::setting::setting::Value;
use user_preferences_api::setting::{Setting, SettingDefinition, SettingKind, SettingWithConstraints};
use user_preferences_api::user_controllable_service::{
    FactoryResetRequest, FactoryResetResponse, RequestSettingsChangeRequest, RequestSettingsChangeResponse,
};
use user_preferences_api::user_preferences_registry_service::{RegisterSettingsRequest, UpdateSettingsRequest};

/// The groups and their schema versions: a new key or a new default is a new version.
const TIME: &str = "time";
const UNITS: &str = "units";
const GROUPS: [(&str, &str); 2] = [(TIME, "1"), (UNITS, "1")];
/// The time zone, an IANA name as UTF-8 bytes (a setting has no string type).
const TIMEZONE: &str = "timezone";
/// The clock shows 24 hours (true) or 12 hours with AM and PM (false).
const HOUR_CYCLE_24: &str = "hour_cycle_24";
/// The unit of temperatures: an enum of two values, Celsius and Fahrenheit.
const TEMPERATURE: &str = "temperature";
const CELSIUS: i32 = 0;
const FAHRENHEIT: i32 = 1;
const DEFAULT_TIMEZONE: &str = "Etc/UTC";
const DEFAULT_HOUR_CYCLE_24: bool = true;
const DEFAULT_TEMPERATURE: i32 = CELSIUS;
/// The system property bionic's localtime reads (system/sepolicy: timezone_prop).
const TIMEZONE_PROPERTY: &str = "persist.sys.timezone";
/// Where bionic looks for the time zone data, in its order.
const TZDATA: [&str; 2] = ["/apex/com.android.tzdata/etc/tz/tzdata", "/system/usr/share/zoneinfo/tzdata"];
/// The clock's format, kept in the bundle's directory (the time zone has its property).
const HOUR_CYCLE_FILE: &str = "hour_cycle_24.pb";
const TEMPERATURE_FILE: &str = "temperature.pb";
/// A kept setting is good for the life of the device.
const KEPT: Duration = Duration::from_secs(100 * 365 * 24 * 3600);
/// Between two attempts to register when user_preferences refused or was not there.
const RETRY: Duration = Duration::from_secs(5);

sdv::lifecycle::register_service_bundle!(SystemServiceBundle);

struct SystemServiceBundle {
    cancellation_token: Option<CancellationToken>,
    context: ContextRef,
    /// The bundle's directory (/data/service_bundle_data/<package>/<bundle>).
    store: Arc<Store>,
}

impl ServiceBundle for SystemServiceBundle {
    fn new(context: ContextRef) -> Self {
        if let Err(err) = sdv::tracing::try_init_tracing() {
            warn!("Failed to init service bundles tracing: {err}");
        }
        let fqin = context.get_self_fqin();
        let store = Arc::new(Store::new(fqin.get_sdv_package_name(), fqin.get_service_bundle_name()));
        Self { cancellation_token: None, context, store }
    }

    fn on_start(&mut self) {
        let context = self.context;
        let store = self.store.clone();
        let cancellation_token = CancellationToken::new();
        self.cancellation_token = Some(cancellation_token.clone());
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_time().enable_io().build() else {
            panic!("Failed to start the runtime!");
        };
        let _ = std::thread::spawn(move || {
            runtime.block_on(async move {
                let comms = Arc::new(SdvComms { context });
                let res = tokio::select! {
                    res = sdv_main(comms, store) => res,
                    () = cancellation_token.cancelled() => Err(SdvStatus::new(SdvStatusCode::Cancelled)),
                };
                error!("Service bundle loop stopped: {res:?}.");
            });
            drop(runtime);
        });
    }

    fn on_stop(&mut self) {
        self.cancellation_token.as_mut().expect("Must have been created on_start").cancel();
        self.cancellation_token = None;
    }
}

async fn sdv_main(comms: Arc<dyn Communicate>, store: Arc<Store>) -> SdvResult<()> {
    info!("Starting service bundle 'SystemServiceBundle'");
    let zones = time_zones();
    let state = State::kept(&store);
    info!("in force: {state:?}");
    let settings = Arc::new(TimeSettings { zones, store, registry: OnceLock::new(), state: Mutex::new(state) });
    let _bundle = Bundle::builder()
        .set_comms(comms.clone())
        .set_server_com_android_sdv_user_preferences_user_controllable_user_controllable_service(settings.clone())
        .build()
        .await
        .map_err(|e| SdvStatus::with_message(SdvStatusCode::Internal, format!("Failed to build service bundle: {e}")))?;
    let registry = Arc::new(registry_client(comms.as_ref()).await?);
    let _ = settings.registry.set(registry.clone());
    // Register, then report the values in force: user_preferences keeps what the owner of a
    // SHARED setting reports. Again later if user_preferences was not ready.
    for (group, version) in GROUPS {
        loop {
            match register(&registry, group, version).await {
                Ok(()) => break,
                Err(e) => warn!("{e}; again in {} s", RETRY.as_secs()),
            }
            tokio::time::sleep(RETRY).await;
        }
        info!("registered the settings group '{group}' version {version}");
    }
    settings.report().await;
    futures::future::pending::<()>().await;
    Ok(())
}

async fn register(registry: &RegistryClient, group: &str, version: &str) -> Result<(), String> {
    registry
        .RegisterSettings(&RegisterSettingsRequest {
            group_name: group.to_string(),
            version: version.to_string(),
            settings_definitions: definitions(group),
            ..Default::default()
        })
        .await
        .map(|_| ())
        .map_err(|e| format!("Failed to register the settings group '{group}': {e}"))
}

/// The client of user_preferences' registry, once its server is registered.
async fn registry_client(comms: &dyn Communicate) -> SdvResult<RegistryClient> {
    let descriptor = ClientDescriptors::<RegistryClient>::COM_ANDROID_SDV_USER_PREFERENCES_USER_PREFERENCES_REGISTRY_SERVICE;
    let mut events = clientlib::create_server_registration_event_stream(comms, descriptor).await?;
    loop {
        match events.next().await {
            Some(Availability::Available) => break,
            Some(_) => debug!("waiting for UserPreferencesRegistryService"),
            None => {
                return Err(SdvStatus::with_message(SdvStatusCode::Unavailable, "the registration events of UserPreferencesRegistryService ended".to_string()))
            }
        }
    }
    clientlib::create_rpc_client(comms, descriptor, sdv::mw::ClientOptions::default()).await
}

/// The group a key belongs to.
fn group_of(key: &str) -> Option<&'static str> {
    match key {
        TIMEZONE | HOUR_CYCLE_24 => Some(TIME),
        TEMPERATURE => Some(UNITS),
        _ => None,
    }
}

/// A group's settings with their defaults; the unit is constrained to its two values.
fn definitions(group: &str) -> Vec<SettingDefinition> {
    let shared = |key: &str, value: Value, constraints: Option<Constraints>| SettingDefinition {
        kind: EnumOrUnknown::new(SettingKind::SHARED),
        setting_with_constraints: MessageField::some(SettingWithConstraints {
            setting: MessageField::some(Setting { key: key.to_string(), value: Some(value), ..Default::default() }),
            constraints,
            ..Default::default()
        }),
        ..Default::default()
    };
    match group {
        TIME => vec![
            shared(TIMEZONE, Value::Blob(DEFAULT_TIMEZONE.as_bytes().to_vec()), None),
            shared(HOUR_CYCLE_24, Value::Bool(DEFAULT_HOUR_CYCLE_24), None),
        ],
        UNITS => vec![shared(
            TEMPERATURE,
            Value::Enum(DEFAULT_TEMPERATURE),
            Some(Constraints::EnumConstraints(EnumConstraints {
                possible_values: vec![CELSIUS, FAHRENHEIT],
                ..Default::default()
            })),
        )],
        _ => Vec::new(),
    }
}

/// The values in force.
#[derive(Clone, Debug)]
struct State {
    timezone: String,
    hour_cycle_24: bool,
    temperature: i32,
}

impl State {
    /// What is in force at start: the system's time zone (empty property: bionic uses UTC), the
    /// clock's format and the unit the bundle kept.
    fn kept(store: &Store) -> Self {
        let timezone = property::get(TIMEZONE_PROPERTY)
            .filter(|tz| !tz.is_empty())
            .unwrap_or_else(|| DEFAULT_TIMEZONE.to_string());
        let hour_cycle_24 = match store.load::<Setting>(HOUR_CYCLE_FILE, KEPT).and_then(|s| s.value) {
            Some(Value::Bool(on)) => on,
            _ => DEFAULT_HOUR_CYCLE_24,
        };
        let temperature = match store.load::<Setting>(TEMPERATURE_FILE, KEPT).and_then(|s| s.value) {
            Some(Value::Enum(unit)) if unit == CELSIUS || unit == FAHRENHEIT => unit,
            _ => DEFAULT_TEMPERATURE,
        };
        Self { timezone, hour_cycle_24, temperature }
    }

    /// A group's values in force.
    fn settings(&self, group: &str) -> Vec<Setting> {
        let setting = |key: &str, value: Value| Setting { key: key.to_string(), value: Some(value), ..Default::default() };
        match group {
            TIME => vec![
                setting(TIMEZONE, Value::Blob(self.timezone.as_bytes().to_vec())),
                setting(HOUR_CYCLE_24, Value::Bool(self.hour_cycle_24)),
            ],
            UNITS => vec![setting(TEMPERATURE, Value::Enum(self.temperature))],
            _ => Vec::new(),
        }
    }
}

struct TimeSettings {
    /// The names in the time zone data; empty when it cannot be read, then only UTC is taken.
    zones: HashSet<String>,
    store: Arc<Store>,
    /// user_preferences' registry, once it is there: the values in force go back to it.
    registry: OnceLock<Arc<RegistryClient>>,
    state: Mutex<State>,
}

impl TimeSettings {
    fn known_zone(&self, name: &str) -> bool {
        if self.zones.is_empty() { name == DEFAULT_TIMEZONE } else { self.zones.contains(name) }
    }

    /// Applies one setting to `state`; a value that cannot be applied leaves it as it is.
    fn apply(&self, state: &mut State, setting: &Setting) {
        match (setting.key.as_str(), &setting.value) {
            (TIMEZONE, Some(Value::Blob(bytes))) => match std::str::from_utf8(bytes) {
                Ok(name) if self.known_zone(name) => {
                    if name == state.timezone {
                        return;
                    }
                    match property::set(TIMEZONE_PROPERTY, name) {
                        Ok(()) => {
                            info!("time zone: {} -> {name}", state.timezone);
                            state.timezone = name.to_string();
                        }
                        Err(e) => error!("time zone {name} not set: {e}"),
                    }
                }
                _ => warn!("not a time zone in the time zone data: {:?}", String::from_utf8_lossy(bytes)),
            },
            (HOUR_CYCLE_24, Some(Value::Bool(on))) => {
                if *on == state.hour_cycle_24 {
                    return;
                }
                match self.store.save(HOUR_CYCLE_FILE, setting) {
                    Ok(()) => {
                        info!("24-hour clock: {on}");
                        state.hour_cycle_24 = *on;
                    }
                    Err(e) => error!("24-hour clock {on} not kept: {e}"),
                }
            }
            (TEMPERATURE, Some(Value::Enum(unit))) if *unit == CELSIUS || *unit == FAHRENHEIT => {
                if *unit == state.temperature {
                    return;
                }
                match self.store.save(TEMPERATURE_FILE, setting) {
                    Ok(()) => {
                        info!("temperature unit: {}", if *unit == FAHRENHEIT { "Fahrenheit" } else { "Celsius" });
                        state.temperature = *unit;
                    }
                    Err(e) => error!("temperature unit {unit} not kept: {e}"),
                }
            }
            (TIMEZONE | HOUR_CYCLE_24 | TEMPERATURE, value) => warn!("{}: not a value it takes: {value:?}", setting.key),
            (key, value) => warn!("not a setting of this bundle: {key} = {value:?}"),
        }
    }

    /// The values in force of every group to user_preferences (UpdateSettings), which sends
    /// them to the screens.
    async fn report(&self) {
        let Some(registry) = self.registry.get() else {
            warn!("the values in force not reported: no registry yet");
            return;
        };
        for (group, _) in GROUPS {
            let settings = self.state.lock().await.settings(group);
            if let Err(e) = registry
                .UpdateSettings(&UpdateSettingsRequest { group_name: group.to_string(), settings, ..Default::default() })
                .await
            {
                warn!("Failed to report the settings in force of '{group}': {e}");
            }
        }
    }
}

#[async_trait]
impl UserControllable for TimeSettings {
    async fn RequestSettingsChange(
        &self,
        _caller_id: sdv::comms::id::ServiceFqin,
        request: &RequestSettingsChangeRequest,
    ) -> SdvResult<RequestSettingsChangeResponse> {
        let group = request.group_name.as_str();
        if !GROUPS.iter().any(|(g, _)| *g == group) {
            return Err(SdvStatus::with_message(SdvStatusCode::NotFound, format!("no settings group {group}")));
        }
        let mut state = self.state.lock().await;
        for setting in &request.settings {
            if group_of(&setting.key) == Some(group) {
                self.apply(&mut state, setting);
            } else {
                warn!("not a setting of '{group}': {}", setting.key);
            }
        }
        // The values in force, which may differ from the request.
        let keys: HashSet<&str> = request.settings.iter().map(|s| s.key.as_str()).collect();
        let settings = state.settings(group).into_iter().filter(|s| keys.contains(s.key.as_str())).collect();
        Ok(RequestSettingsChangeResponse { settings, ..Default::default() })
    }

    /// Every group back to its defaults, then the values in force to user_preferences (the
    /// proto's contract; the request names no group).
    async fn FactoryReset(
        &self,
        _caller_id: sdv::comms::id::ServiceFqin,
        _request: &FactoryResetRequest,
    ) -> SdvResult<FactoryResetResponse> {
        {
            let mut state = self.state.lock().await;
            for (group, _) in GROUPS {
                for definition in definitions(group) {
                    if let Some(setting) = definition.setting_with_constraints.setting.as_ref() {
                        self.apply(&mut state, setting);
                    }
                }
            }
            info!("factory reset: {state:?}");
        }
        self.report().await;
        Ok(FactoryResetResponse::new())
    }
}

/// The time zone names in bionic's time zone data (bionic/libc/tzcode/bionic.cpp): a header (12
/// bytes of version, then the offsets of the index, the data and the final part, big endian),
/// then the index, entries of 52 bytes that begin with the name (40 bytes, NUL padded).
fn time_zones() -> HashSet<String> {
    let Some(data) = TZDATA.iter().find_map(|path| std::fs::read(path).ok()) else {
        warn!("no time zone data: only {DEFAULT_TIMEZONE} is taken");
        return HashSet::new();
    };
    let be = |at: usize| data.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let zones: HashSet<String> = match (be(12), be(16)) {
        (Some(index), Some(end)) => data
            .get(index..end)
            .map(|entries| {
                entries
                    .chunks_exact(52)
                    .filter_map(|entry| {
                        let name = &entry[..40];
                        let len = name.iter().position(|&b| b == 0).unwrap_or(40);
                        std::str::from_utf8(&name[..len]).ok().map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => HashSet::new(),
    };
    if zones.is_empty() {
        warn!("the time zone data has no index: only {DEFAULT_TIMEZONE} is taken");
    } else {
        info!("{} time zones in the time zone data", zones.len());
    }
    zones
}

/// bionic's system properties: rustutils has no system_properties in an APEX's variant.
mod property {
    use std::ffi::{c_char, c_int, CStr, CString};

    /// PROP_VALUE_MAX of <sys/system_properties.h>.
    const VALUE_MAX: usize = 92;

    extern "C" {
        fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
        fn __system_property_set(name: *const c_char, value: *const c_char) -> c_int;
    }

    pub fn get(name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        let mut value = [0 as c_char; VALUE_MAX];
        // SAFETY: name is NUL terminated, value has room for PROP_VALUE_MAX bytes.
        unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };
        // SAFETY: bionic writes a NUL terminated string into value.
        Some(unsafe { CStr::from_ptr(value.as_ptr()) }.to_string_lossy().into_owned())
    }

    pub fn set(name: &str, value: &str) -> Result<(), String> {
        let (Ok(name), Ok(value)) = (CString::new(name), CString::new(value)) else {
            return Err("NUL in the name or the value".to_string());
        };
        // SAFETY: both strings are NUL terminated and live across the call.
        match unsafe { __system_property_set(name.as_ptr(), value.as_ptr()) } {
            0 => Ok(()),
            rc => Err(format!("__system_property_set returned {rc} (SELinux or init refused it)")),
        }
    }
}
