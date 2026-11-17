//! The link to the screen, over binder: the panel serves com.everyware.screen.IScreenLink/default
//! (interfaces/screen) and takes a call only from this bundle (it asks the SDV identity agent who
//! the calling process is). Binder and not a port on 127.0.0.1, because any local process can
//! reach a port, and the screen's commands carry the password typed on it.
//!
//! The panel is looked for once a second while it is not there; once found, a death recipient
//! says at once when its process is gone (as HAR's bundle reconnects when its stream breaks).

use binder::binder_impl::IBinderInternal;
use binder::{BinderFeatures, DeathRecipient, IBinder, Interface, Strong};
use com_everyware_screen::aidl::com::everyware::screen::ICommandSink::{BnCommandSink, ICommandSink};
use com_everyware_screen::aidl::com::everyware::screen::IScreenLink::IScreenLink;
use log::{info, warn};
use protobuf::Message;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;

/// The name the panel serves under (smarthome-app src/screen_binder.rs).
const SERVICE: &str = "com.everyware.screen.IScreenLink/default";

/// A command of the screen, as the panel sent it.
pub enum ScreenCommand {
    Wifi(Vec<u8>),
    Matter(Vec<u8>),
    Settings(Vec<u8>),
    Calendar(Vec<u8>),
}

/// The panel's end while it is there, and this bundle's command sink.
pub struct Screen {
    link: Mutex<Option<Linked>>,
    sink: Strong<dyn ICommandSink>,
    /// Told the generation of a panel whose process died.
    deaths: UnboundedSender<u64>,
    /// Counts the panels found: a late death of an earlier one is not taken for the current one.
    generation: AtomicU64,
}

/// The panel found, with the death recipient that watches it (dropping it unlinks).
struct Linked {
    link: Strong<dyn IScreenLink>,
    generation: u64,
    _death: DeathRecipient,
    /// The panel took the sink; until then every tick tries again (the panel refuses while it
    /// cannot ask the identity agent, for one).
    subscribed: bool,
}

impl Screen {
    /// The screen's commands go to `commands`; the generation of a panel that died to `deaths`
    /// (hand it back to `on_death`).
    pub fn new(commands: UnboundedSender<ScreenCommand>, deaths: UnboundedSender<u64>) -> Self {
        let sink = BnCommandSink::new_binder(Sink { commands }, BinderFeatures::default());
        Screen { link: Mutex::new(None), sink, deaths, generation: AtomicU64::new(0) }
    }

    /// Looks for the panel when it is not there, and hands it the sink until it takes it;
    /// true: the panel took the sink just now. A panel that was found starts from nothing (it
    /// started or restarted), and one that refused until now (it could not ask the identity
    /// agent, for one) refused every value too: either way it needs every last value now.
    ///
    /// Blocking binder calls on the bundle's current-thread runtime: each is a local call to a
    /// panel that answers at once, once a tick at most while the panel is not linked.
    pub fn connect(&self) -> bool {
        let mut guard = self.link.lock().expect("screen link");
        if let Some(linked) = guard.as_mut() {
            if linked.link.as_binder().is_binder_alive() {
                if linked.subscribed {
                    return false;
                }
                linked.subscribed = subscribe(&linked.link, &self.sink);
                return linked.subscribed;
            }
            *guard = None;
        }
        let Ok(link) = binder::check_interface::<dyn IScreenLink>(SERVICE) else { return false };
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let deaths = self.deaths.clone();
        // on a binder thread: only a message to the bundle's runtime
        let mut death = DeathRecipient::new(move || {
            let _ = deaths.send(generation);
        });
        if let Err(err) = link.as_binder().link_to_death(&mut death) {
            // it died between the lookup and now: the next tick finds the new one
            warn!("the panel is gone already ({err})");
            return false;
        }
        info!("the panel serves {SERVICE}");
        let subscribed = subscribe(&link, &self.sink);
        *guard = Some(Linked { link, generation, _death: death, subscribed });
        subscribed
    }

    /// The panel of this generation died: forget it, if it is the current one.
    pub fn on_death(&self, generation: u64) {
        let mut guard = self.link.lock().expect("screen link");
        if guard.as_ref().is_some_and(|linked| linked.generation == generation) {
            *guard = None;
            warn!("the panel is gone, keeping the last values");
        }
    }

    pub fn publish_weather<M: Message>(&self, weather: &M) {
        self.publish("weather", weather, |link, bytes| link.publishWeather(bytes));
    }

    pub fn publish_news<M: Message>(&self, news: &M) {
        self.publish("news", news, |link, bytes| link.publishNews(bytes));
    }

    pub fn publish_thumbnail<M: Message>(&self, thumbnail: &M) {
        self.publish("thumbnail", thumbnail, |link, bytes| link.publishNewsThumbnail(bytes));
    }

    pub fn publish_networks<M: Message>(&self, networks: &M) {
        self.publish("Wi-Fi networks", networks, |link, bytes| link.publishWifiNetworks(bytes));
    }

    pub fn publish_state<M: Message>(&self, state: &M) {
        self.publish("Wi-Fi state", state, |link, bytes| link.publishWifiState(bytes));
    }

    pub fn publish_wifi_result<M: Message>(&self, command: &str, ssid: &str, result: &M) {
        self.publish("Wi-Fi result", result, |link, bytes| link.publishWifiResult(command, ssid, bytes));
    }

    pub fn publish_matter_devices<M: Message>(&self, devices: &M) {
        self.publish("Matter devices", devices, |link, bytes| link.publishMatterDevices(bytes));
    }

    pub fn publish_matter_result<M: Message>(&self, command: &str, node_id: u64, endpoint: u32, result: &M) {
        self.publish("Matter result", result, |link, bytes| {
            link.publishMatterResult(command, node_id as i64, endpoint as i32, bytes)
        });
    }

    pub fn publish_settings<M: Message>(&self, settings: &M) {
        self.publish("settings", settings, |link, bytes| link.publishSettings(bytes));
    }

    pub fn publish_agenda<M: Message>(&self, agenda: &M) {
        self.publish("agenda", agenda, |link, bytes| link.publishAgenda(bytes));
    }

    /// One call to the panel; dropped while it is not linked (connect sends the last values).
    fn publish<M: Message>(&self, what: &str, message: &M, call: impl FnOnce(&Strong<dyn IScreenLink>, &[u8]) -> binder::Result<()>) {
        // Only to a panel that took the sink: it takes a value on the same caller check, and
        // connect sends every last value once it took the sink. Not held across the call: a call
        // to a busy panel must not hold up the others' lookups.
        let link = self.link.lock().expect("screen link").as_ref().filter(|l| l.subscribed).map(|l| l.link.clone());
        let Some(link) = link else { return };
        let Ok(bytes) = message.write_to_bytes() else { return };
        if let Err(err) = call(&link, &bytes) {
            warn!("publish {what}: {err}");
        }
    }
}

/// Hands the panel the command sink; true: it took it.
fn subscribe(link: &Strong<dyn IScreenLink>, sink: &Strong<dyn ICommandSink>) -> bool {
    match link.subscribeCommands(sink) {
        Ok(()) => {
            info!("taking the screen's commands");
            true
        }
        Err(err) => {
            warn!("the panel did not take the command sink: {err}; again in a second");
            false
        }
    }
}

/// The sink the panel calls with each command, on a binder thread: into the bundle's runtime.
struct Sink {
    commands: UnboundedSender<ScreenCommand>,
}

impl Interface for Sink {}

impl ICommandSink for Sink {
    fn onWifiCommand(&self, command: &[u8]) -> binder::Result<()> {
        let _ = self.commands.send(ScreenCommand::Wifi(command.to_vec()));
        Ok(())
    }

    fn onMatterCommand(&self, command: &[u8]) -> binder::Result<()> {
        let _ = self.commands.send(ScreenCommand::Matter(command.to_vec()));
        Ok(())
    }

    fn onSettingsChange(&self, request: &[u8]) -> binder::Result<()> {
        let _ = self.commands.send(ScreenCommand::Settings(request.to_vec()));
        Ok(())
    }

    fn onCalendarCommand(&self, command: &[u8]) -> binder::Result<()> {
        let _ = self.commands.send(ScreenCommand::Calendar(command.to_vec()));
        Ok(())
    }
}
