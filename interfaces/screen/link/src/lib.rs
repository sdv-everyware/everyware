//! The screen's side of the link to the house's data, over binder. The dashboard bundle
//! (bundles/dashboard) subscribes to the house's topics and calls
//! com.everyware.screen.IScreenLink/default (interfaces/screen): it pushes the weather, the news,
//! the agenda, the Wi-Fi networks and state, the Matter devices, the device's settings and the
//! answers to the screen's commands, and hands in the sink the screen's commands go to.
//!
//! An app implements [`Screen`] for what its screen shows (every handler has a default that
//! drops the message) and calls [`spawn`]. One app serves the name at a time.
//!
//! The commands carry the password typed on the screen, so the server takes a call only from
//! the dashboard bundle: the kernel gives the caller's pid and uid with every call, and the SDV
//! identity agent says which service bundle that process is.
//!
//! Whether the link is up comes from binder too: a death recipient on the bundle's sink says
//! when the bundle's process is gone, and the next subscribe that it is back. No heartbeat.

use binder::binder_impl::IBinderInternal;
use binder::{BinderFeatures, DeathRecipient, ExceptionCode, IBinder, Interface, Status, StatusCode, Strong, ThreadState};
use com_everyware_screen::aidl::com::everyware::screen::ICommandSink::ICommandSink;
use com_everyware_screen::aidl::com::everyware::screen::IScreenLink::{BnScreenLink, IScreenLink};
use everyware_catalog::calendar::Agenda;
use everyware_catalog::calendar_service::{CalendarCommand, CalendarResult};
use everyware_catalog::matter::MatterDevices;
use everyware_catalog::matter_service::{MatterCommand, MatterResult};
use everyware_catalog::news::{NewsFeed, NewsThumbnail};
use everyware_catalog::weather::Weather;
use everyware_catalog::wifi::{WifiCommand, WifiNetworks, WifiState};
use everyware_catalog::wifi_service::WifiResult;
use google_sdv_identity::aidl::google::sdv::identity::IIdentityAgent::IIdentityAgent;
use google_sdv_identity::aidl::google::sdv::identity::ProcessIdentifier::ProcessIdentifier;
use protobuf::Message;
use std::sync::{Mutex, OnceLock};
use std::thread;
use user_preferences_api::change_notifier::OnSettingsChangeRequest;
use user_preferences_api::user_preferences_management_service::RequestSettingsChangeRequest;

/// The name the screen serves under (a VINTF fragment on the device declares it).
pub const SERVICE: &str = "com.everyware.screen.IScreenLink/default";
/// The SDV identity agent: it knows which service bundle runs in which process.
const IDENTITY_AGENT: &str = "google.sdv.identity.IIdentityAgent/default";
/// The only caller: the dashboard bundle (bundles/dashboard/apex, its service bundles manifest).
const CALLER_PACKAGE: &str = "com.everyware.dashboard";
const CALLER_BUNDLE: &str = "DashboardServiceBundle";

/// The Wi-Fi bundle's answer to a command of the screen (IScreenLink.publishWifiResult).
#[derive(Clone, Debug, PartialEq)]
pub struct WifiCommandResult {
    /// "scan", "connect", "forget", "disconnect".
    pub command: String,
    /// The SSID of a connect or forget.
    pub ssid: String,
    /// What the Wi-Fi bundle answered.
    pub result: WifiResult,
}

/// The Matter bundle's answer to a command of the screen (IScreenLink.publishMatterResult).
#[derive(Clone, Debug, PartialEq)]
pub struct MatterCommandResult {
    /// "pair", "on-off", "level", "setpoint", "system-mode", "fan-mode".
    pub command: String,
    /// The device the command went to (0 for a pairing).
    pub node_id: u64,
    /// The device's endpoint.
    pub endpoint: u32,
    /// What the Matter bundle answered.
    pub result: MatterResult,
}

/// The calendar bundle's answer to a command of the screen (IScreenLink.publishCalendarResult).
#[derive(Clone, Debug, PartialEq)]
pub struct CalendarCommandResult {
    /// Which command was answered.
    pub command: String,
    /// The event the command was about.
    pub event_id: String,
    /// What the calendar bundle answered.
    pub result: CalendarResult,
}

/// What a screen does with the house's data. Every handler has a default that drops the
/// message, so an app writes only the ones its screen shows, and a message added to the link
/// later breaks no app.
///
/// The handlers run on binder's threads, some of them while the link holds its own lock: a
/// handler hands the message over (an action to the app's reducer) and returns. It does not
/// call a `send_*` of this crate.
pub trait Screen: Send + Sync + 'static {
    /// The dashboard bundle took the screen's commands (true), or its process is gone (false).
    fn link(&self, _up: bool) {}
    /// The weather of the house.
    fn weather(&self, _weather: Weather) {}
    /// The news feed.
    fn news(&self, _news: NewsFeed) {}
    /// The thumbnail of a story of the feed.
    fn news_thumbnail(&self, _thumbnail: NewsThumbnail) {}
    /// The agenda of the linked calendar.
    fn agenda(&self, _agenda: Agenda) {}
    /// The Wi-Fi networks in range.
    fn wifi_networks(&self, _networks: WifiNetworks) {}
    /// The Wi-Fi link.
    fn wifi_state(&self, _state: WifiState) {}
    /// The Wi-Fi bundle's answer to a command of the screen.
    fn wifi_result(&self, _result: WifiCommandResult) {}
    /// The Matter devices of the house.
    fn matter_devices(&self, _devices: MatterDevices) {}
    /// The Matter bundle's answer to a command of the screen.
    fn matter_result(&self, _result: MatterCommandResult) {}
    /// The calendar bundle's answer to a command of the screen.
    fn calendar_result(&self, _result: CalendarCommandResult) {}
    /// The device's settings, as their owner applied them.
    fn settings(&self, _settings: OnSettingsChangeRequest) {}
}

/// The sink the screen's commands go to, with the death recipient that watches it.
struct Subscriber {
    sink: Strong<dyn ICommandSink>,
    _death: DeathRecipient,
    /// The process (pid, uid) that handed in the sink, after the identity agent named it the
    /// dashboard bundle: its calls pass without asking the agent again, as long as the sink
    /// lives. Any other caller is asked every time (the bundle subscribes before it publishes,
    /// so only a stranger pays for that). Not the uid alone: uid 0 is shared by many processes.
    /// None once the sink died: a pid is reused, and the next process with it must be asked.
    trusted: Option<(i32, u32)>,
}

static SUBSCRIBER: Mutex<Option<Subscriber>> = Mutex::new(None);

/// The app's handlers (set by spawn).
static SCREEN: OnceLock<Box<dyn Screen>> = OnceLock::new();

fn screen() -> Option<&'static dyn Screen> {
    SCREEN.get().map(|screen| screen.as_ref())
}

/// The bundle's process is gone: the screen is told. Called with SUBSCRIBER held, so a new
/// subscribe cannot get in between and be followed by this.
fn bundle_gone() {
    if let Some(screen) = screen() {
        screen.link(false);
    }
}

/// Sends a Wi-Fi command to the dashboard bundle; false: it did not go out (logged).
pub fn send_wifi_command(command: WifiCommand) -> bool {
    send("Wi-Fi", &command, |sink, bytes| sink.onWifiCommand(bytes))
}

/// Sends a Matter command to the dashboard bundle; false: it did not go out (logged).
pub fn send_matter_command(command: MatterCommand) -> bool {
    send("Matter", &command, |sink, bytes| sink.onMatterCommand(bytes))
}

/// Sends a change of the calendar (an answer, a deletion, the unlink) to the dashboard bundle;
/// false: it did not go out (logged).
pub fn send_calendar_command(command: CalendarCommand) -> bool {
    send("calendar", &command, |sink, bytes| sink.onCalendarCommand(bytes))
}

/// Asks for a change of the device's settings; the values come back with `Screen::settings`
/// once the group's owner applied them. False: it did not go out (logged).
pub fn send_settings_change(request: RequestSettingsChangeRequest) -> bool {
    send("settings", &request, |sink, bytes| sink.onSettingsChange(bytes))
}

fn send<M: Message>(what: &str, command: &M, call: impl FnOnce(&Strong<dyn ICommandSink>, &[u8]) -> binder::Result<()>) -> bool {
    let Ok(bytes) = command.write_to_bytes() else {
        log::error!("screen link: cannot encode a {what} command");
        return false;
    };
    let mut subscriber = SUBSCRIBER.lock().expect("subscriber");
    let Some(current) = subscriber.as_ref() else {
        log::warn!("screen link: no command sink, the {what} command is dropped");
        return false;
    };
    match call(&current.sink, &bytes) {
        Ok(()) => true,
        // only a dead sink goes: a full async buffer fails a oneway call too
        // the death notice may come later, or not at all for this sink once it is dropped here
        Err(err) if err.transaction_error() == StatusCode::DEAD_OBJECT => {
            log::warn!("screen link: the command sink is gone");
            *subscriber = None;
            bundle_gone();
            false
        }
        Err(err) => {
            log::warn!("screen link: a {what} command did not go out ({err})");
            false
        }
    }
}

struct ScreenLink {
    screen: &'static dyn Screen,
}

impl Interface for ScreenLink {}

impl ScreenLink {
    /// The caller is the dashboard bundle, or the call fails with EX_SECURITY; Ok: its pid and
    /// uid.
    fn check_caller(&self) -> binder::Result<(i32, u32)> {
        let (pid, uid) = (ThreadState::get_calling_pid(), ThreadState::get_calling_uid());
        // not held across the agent's call: other callers must not queue behind it
        if SUBSCRIBER.lock().expect("subscriber").as_ref().is_some_and(|s| s.trusted == Some((pid, uid))) {
            return Ok((pid, uid));
        }
        let refuse = |why: String| {
            log::warn!("screen link: refused pid {pid} uid {uid}: {why}");
            Err(Status::new_exception_str(ExceptionCode::SECURITY, Some("not the dashboard bundle")))
        };
        let agent = match binder::check_interface::<dyn IIdentityAgent>(IDENTITY_AGENT) {
            Ok(agent) => agent,
            Err(err) => return refuse(format!("no identity agent ({err})")),
        };
        let process = ProcessIdentifier { pid: pid.into(), uid: uid.into() };
        let identity = match agent.findIdentityByProcess(&process) {
            Ok(Some(identity)) => identity,
            Ok(None) => return refuse("no SDV identity".into()),
            Err(err) => return refuse(format!("identity agent failed ({err})")),
        };
        let fqin = &identity.name;
        if fqin.sdvPackageName != CALLER_PACKAGE || fqin.serviceBundleName != CALLER_BUNDLE {
            return refuse(format!("{}.{}", fqin.sdvPackageName, fqin.serviceBundleName));
        }
        log::info!("screen link: the dashboard bundle is uid {uid} ({}/{})", fqin.serviceBundleName, fqin.serviceInstanceName);
        Ok((pid, uid))
    }
}

/// A message the bundle sent, or EX_ILLEGAL_ARGUMENT.
fn parse<M: Message>(bytes: &[u8], what: &str) -> binder::Result<M> {
    M::parse_from_bytes(bytes).map_err(|err| {
        log::warn!("screen link: a {what} that does not parse: {err}");
        Status::new_exception_str(ExceptionCode::ILLEGAL_ARGUMENT, Some(what))
    })
}

impl IScreenLink for ScreenLink {
    fn publishWeather(&self, weather: &[u8]) -> binder::Result<()> {
        self.check_caller()?;
        self.screen.weather(parse::<Weather>(weather, "Weather")?);
        Ok(())
    }

    fn publishNews(&self, news: &[u8]) -> binder::Result<()> {
        self.check_caller()?;
        self.screen.news(parse::<NewsFeed>(news, "NewsFeed")?);
        Ok(())
    }

    fn publishNewsThumbnail(&self, thumbnail: &[u8]) -> binder::Result<()> {
        self.check_caller()?;
        self.screen.news_thumbnail(parse::<NewsThumbnail>(thumbnail, "NewsThumbnail")?);
        Ok(())
    }

    fn publishAgenda(&self, agenda: &[u8]) -> binder::Result<()> {
        self.check_caller()?;
        self.screen.agenda(parse::<Agenda>(agenda, "Agenda")?);
        Ok(())
    }

    fn publishWifiNetworks(&self, networks: &[u8]) -> binder::Result<()> {
        self.check_caller()?;
        self.screen.wifi_networks(parse::<WifiNetworks>(networks, "WifiNetworks")?);
        Ok(())
    }

    fn publishWifiState(&self, state: &[u8]) -> binder::Result<()> {
        self.check_caller()?;
        self.screen.wifi_state(parse::<WifiState>(state, "WifiState")?);
        Ok(())
    }

    fn publishWifiResult(&self, command: &str, ssid: &str, result: &[u8]) -> binder::Result<()> {
        self.check_caller()?;
        let result = parse::<WifiResult>(result, "WifiResult")?;
        self.screen.wifi_result(WifiCommandResult { command: command.to_string(), ssid: ssid.to_string(), result });
        Ok(())
    }

    fn publishMatterDevices(&self, devices: &[u8]) -> binder::Result<()> {
        self.check_caller()?;
        self.screen.matter_devices(parse::<MatterDevices>(devices, "MatterDevices")?);
        Ok(())
    }

    fn publishMatterResult(&self, command: &str, node_id: i64, endpoint: i32, result: &[u8]) -> binder::Result<()> {
        self.check_caller()?;
        let result = parse::<MatterResult>(result, "MatterResult")?;
        // the ids are unsigned in the catalog; AIDL has no unsigned types
        let message = MatterCommandResult { command: command.to_string(), node_id: node_id as u64, endpoint: endpoint as u32, result };
        self.screen.matter_result(message);
        Ok(())
    }

    fn publishCalendarResult(&self, command: &str, event_id: &str, result: &[u8]) -> binder::Result<()> {
        self.check_caller()?;
        let result = parse::<CalendarResult>(result, "CalendarResult")?;
        self.screen.calendar_result(CalendarCommandResult { command: command.to_string(), event_id: event_id.to_string(), result });
        Ok(())
    }

    fn publishSettings(&self, settings: &[u8]) -> binder::Result<()> {
        self.check_caller()?;
        self.screen.settings(parse::<OnSettingsChangeRequest>(settings, "OnSettingsChangeRequest")?);
        Ok(())
    }

    fn subscribeCommands(&self, sink: &Strong<dyn ICommandSink>) -> binder::Result<()> {
        let caller = self.check_caller()?;
        let mut subscriber = SUBSCRIBER.lock().expect("subscriber");
        if subscriber.as_ref().is_some_and(|current| current.sink.as_binder() == sink.as_binder()) {
            // the bundle asks again (it did not see the first answer): nothing changes
            return Ok(());
        }
        if subscriber.as_ref().is_some_and(|current| current.sink.as_binder().is_binder_alive()) {
            log::warn!("screen link: a second command sink is refused");
            return Err(Status::new_exception_str(ExceptionCode::ILLEGAL_STATE, Some("a sink is subscribed")));
        }
        // On a binder thread when the bundle's process died. Only this sink's death counts: a
        // late one of a sink already replaced says nothing about the link.
        let watched = sink.as_binder();
        // The sink stays (dropping it here would drop this recipient inside its own call): only
        // its trust goes, and the next subscribe replaces the dead sink.
        let mut death = DeathRecipient::new(move || {
            let mut subscriber = SUBSCRIBER.lock().expect("subscriber");
            if let Some(current) = subscriber.as_mut().filter(|s| s.sink.as_binder() == watched) {
                log::warn!("screen link: the dashboard bundle is gone");
                current.trusted = None;
                bundle_gone();
            }
        });
        if sink.as_binder().link_to_death(&mut death).is_err() {
            log::warn!("screen link: the command sink died before it was taken");
            return Err(StatusCode::DEAD_OBJECT.into());
        }
        *subscriber = Some(Subscriber { sink: sink.clone(), _death: death, trusted: Some(caller) });
        log::info!("screen link: the bundle takes the screen's commands");
        self.screen.link(true);
        Ok(())
    }
}

/// Registers the service with the app's handlers and serves it on binder's thread pool;
/// returns at once. Once per process: a second call is logged and does nothing.
pub fn spawn(handlers: impl Screen) {
    if SCREEN.set(Box::new(handlers)).is_err() {
        log::error!("screen link: spawned twice, the second screen is dropped");
        return;
    }
    thread::spawn(move || {
        let Some(screen) = screen() else { return };
        let binder = BnScreenLink::new_binder(ScreenLink { screen }, BinderFeatures::default());
        // the pool first, then the registration (as the Thread HAL's service does)
        binder::ProcessState::start_thread_pool();
        if let Err(err) = binder::add_service(SERVICE, binder.as_binder()) {
            log::error!("screen link: cannot register {SERVICE}: {err}");
            return;
        }
        log::info!("screen link: serving {SERVICE}");
        binder::ProcessState::join_thread_pool();
    });
}
