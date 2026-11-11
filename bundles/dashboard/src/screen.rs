//! The Wi-Fi part of the link to the screen, over binder: the panel serves
//! com.everyware.screen.IScreenLink/default (interfaces/screen) and takes a call only from this
//! bundle (it asks the SDV identity agent who the calling process is). Binder and not the gRPC
//! port, because the screen's Wi-Fi commands carry the password typed on it, and any local
//! process can reach a port on 127.0.0.1.

use binder::{BinderFeatures, IBinder, Interface, Strong};
use com_everyware_screen::aidl::com::everyware::screen::IScreenLink::IScreenLink;
use com_everyware_screen::aidl::com::everyware::screen::IWifiCommandSink::{BnWifiCommandSink, IWifiCommandSink};
use log::{info, warn};
use protobuf::Message;
use std::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;

/// The name the panel serves under (smarthome-app src/screen_binder.rs).
const SERVICE: &str = "com.everyware.screen.IScreenLink/default";

/// The panel's end, while it is there, and this bundle's command sink, once the Wi-Fi bundle's
/// service is there to call.
#[derive(Default)]
pub struct Screen {
    link: Mutex<Option<Strong<dyn IScreenLink>>>,
    sink: Mutex<Option<Strong<dyn IWifiCommandSink>>>,
    /// The panel took the sink; until then every tick tries again (the panel refuses while it
    /// cannot ask the identity agent, for one).
    subscribed: Mutex<bool>,
}

impl Screen {
    /// The panel's end, if it is alive.
    fn alive(&self) -> Option<Strong<dyn IScreenLink>> {
        let link = self.link.lock().expect("screen link").clone()?;
        link.as_binder().ping_binder().ok().map(|()| link)
    }

    /// Looks for the panel when it is not there, and hands it the sink until it takes it;
    /// true: the panel was found just now (it started or restarted, and starts from nothing).
    pub fn connect(&self) -> bool {
        if self.alive().is_some() {
            if !*self.subscribed.lock().expect("subscribed") {
                self.subscribe();
            }
            return false;
        }
        let Ok(link) = binder::check_interface::<dyn IScreenLink>(SERVICE) else {
            *self.link.lock().expect("screen link") = None;
            return false;
        };
        info!("the panel serves {SERVICE}");
        *self.link.lock().expect("screen link") = Some(link);
        *self.subscribed.lock().expect("subscribed") = false;
        self.subscribe();
        true
    }

    /// Hands the panel the command sink, if there is one yet.
    fn subscribe(&self) {
        let Some(link) = self.alive() else { return };
        let Some(sink) = self.sink.lock().expect("command sink").clone() else { return };
        match link.subscribeWifiCommands(&sink) {
            Ok(()) => {
                info!("taking Wi-Fi commands from the panel");
                *self.subscribed.lock().expect("subscribed") = true;
            }
            Err(err) => warn!("the panel did not take the command sink: {err}; again in a second"),
        }
    }

    /// The screen's Wi-Fi commands go to `commands` from now on.
    pub fn take_commands(&self, commands: UnboundedSender<Vec<u8>>) {
        let sink = BnWifiCommandSink::new_binder(Sink { commands }, BinderFeatures::default());
        *self.sink.lock().expect("command sink") = Some(sink);
        self.subscribe();
    }

    pub fn publish_networks<M: Message>(&self, networks: &M) {
        self.publish("Wi-Fi networks", networks, |link, bytes| link.publishWifiNetworks(bytes));
    }

    pub fn publish_state<M: Message>(&self, state: &M) {
        self.publish("Wi-Fi state", state, |link, bytes| link.publishWifiState(bytes));
    }

    pub fn publish_result<M: Message>(&self, command: &str, ssid: &str, result: &M) {
        self.publish("Wi-Fi result", result, |link, bytes| link.publishWifiResult(command, ssid, bytes));
    }

    /// One call to the panel; dropped while it is not there (connect sends the last values).
    fn publish<M: Message>(&self, what: &str, message: &M, call: impl FnOnce(&Strong<dyn IScreenLink>, &[u8]) -> binder::Result<()>) {
        let Some(link) = self.alive() else { return };
        let Ok(bytes) = message.write_to_bytes() else { return };
        if let Err(err) = call(&link, &bytes) {
            warn!("publish {what}: {err}");
        }
    }
}

/// The sink the panel calls with each command, on a binder thread: into the bundle's runtime.
struct Sink {
    commands: UnboundedSender<Vec<u8>>,
}

impl Interface for Sink {}

impl IWifiCommandSink for Sink {
    fn onCommand(&self, command: &[u8]) -> binder::Result<()> {
        let _ = self.commands.send(command.to_vec());
        Ok(())
    }
}
