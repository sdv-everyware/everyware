//! WifiServiceBundle: owns the Wi-Fi of the house. It talks to wpa_supplicant over its
//! control interface (wpa_ctrl.rs), scans, publishes the networks around (`wifi-networks`)
//! and the state of the connection (`wifi-state`), and offers the WifiService RPC
//! (`catalog/wifi_service.proto`: scan, connect, forget, disconnect, each call answered when
//! it is done). The screen never sees the supplicant: the dashboard bundle bridges the
//! topics to the harry process and calls the service for it. README.md says why it is built
//! this way and what the security of it rests on.
//!
//! The shape is the service bundle vsidlc generates (`vsidlc --services`): a `ServiceBundle`
//! with `SdvComms`, a current-thread runtime, `sdv_main` with the bundle's publishers and
//! the RPC server; the business logic is in `Session`, which the server reaches through a
//! channel (one task owns the supplicant connection).

mod wpa_ctrl;

use everyware_catalog::wifi::{WifiConnect, WifiConnection, WifiDisconnect, WifiForget, WifiNetwork, WifiNetworks, WifiScan, WifiSecurity, WifiState};
use everyware_catalog::wifi_service::WifiResult;
use log::{debug, error, info, warn};
use sdv::mw::{clientlib, Communicate, Publisher, SdvComms};
use sdv::status::{SdvResult, SdvStatus, SdvStatusCode};
use sdv_mw_rs_com_everyware_wifi_service_bundle::{PublisherDescriptors, WifiServiceBundle as Bundle};
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval, sleep, timeout};
use tokio_util::sync::CancellationToken;
use wpa_ctrl::{bars, parse_networks, parse_scan_results, status_value, Ctrl, GLOBAL_CTRL, IFACE};

/// Android's wpa_supplicant answers a plain `STATUS` and, as a side effect, sends every
/// attached monitor a fresh CTRL-EVENT-STATE-CHANGE and CTRL-EVENT-CONNECTED (ctrl_iface.c,
/// `#ifdef ANDROID`: the framework learns the state that way). A client that asks for the
/// status on CONNECTED would loop with it at full speed; `-NO_EVENTS` is the same answer
/// without the events.
const STATUS: &str = "STATUS-NO_EVENTS";

/// Between scans on the bundle's own (the supplicant scans only on request; the screen asks
/// for one when its sheet opens). A scan makes the supplicant reconsider its connection, so
/// not too often.
const SCAN_INTERVAL: Duration = Duration::from_secs(60);
/// A network missing from one scan leaves the supplicant's cache (BSS_EXPIRE_COUNT; its
/// default 2 keeps a switched-off hotspot for two scans). The age, checked on a timer of its
/// own, is just above the scan interval so networks still there are not dropped between scans.
const BSS_EXPIRE_COUNT: u32 = 1;
const BSS_EXPIRE_AGE: Duration = Duration::from_secs(SCAN_INTERVAL.as_secs() + 10);
/// The supplicant starts after the Wi-Fi driver is loaded, later than this bundle.
const RETRY_INTERVAL: Duration = Duration::from_secs(2);
/// A connect is answered within this: the supplicant tries a wrong password three times
/// (~15 s) before it gives up on the network.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(35);
/// A scan is answered within this.
const SCAN_TIMEOUT: Duration = Duration::from_secs(15);

sdv::lifecycle::register_service_bundle!(WifiServiceBundle);

struct WifiServiceBundle {
    cancellation_token: Option<CancellationToken>,
    context: ContextRef,
}

impl ServiceBundle for WifiServiceBundle {
    fn new(context: ContextRef) -> Self {
        if let Err(err) = sdv::tracing::try_init_tracing() {
            warn!("Failed to init service bundles tracing: {err}");
        }
        Self { cancellation_token: None, context }
    }

    fn on_start(&mut self) {
        let context = self.context;
        let cancellation_token = CancellationToken::new();
        self.cancellation_token = Some(cancellation_token.clone());
        let Ok(runtime) =
            tokio::runtime::Builder::new_current_thread().enable_time().enable_io().build()
        else {
            panic!("Failed to start the runtime!");
        };
        let _ = std::thread::spawn(move || {
            runtime.block_on(async move {
                let comms = Arc::new(SdvComms { context });
                let res = tokio::select! {
                    res = sdv_main(comms) => res,
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

async fn sdv_main(comms: Arc<dyn Communicate>) -> SdvResult<()> {
    info!("Starting service bundle 'WifiServiceBundle'");
    let (requests_tx, requests_rx) = mpsc::channel(16);
    let _service_bundle = match Bundle::builder()
        .set_comms(comms.clone())
        .set_server_wifi(Arc::new(WifiServiceRpc { requests: requests_tx }))
        .build()
        .await
    {
        Ok(service) => service,
        Err(e) => {
            error!("Failed to build service bundle: {e}");
            return Err(SdvStatus::with_message(SdvStatusCode::Internal, e.to_string()));
        }
    };
    let networks = clientlib::create_publisher(comms.as_ref(), PublisherDescriptors::<WifiNetworks>::WIFI_NETWORKS).await.map_err(|e| {
        error!("Failed to create publisher WifiNetworks#wifi-networks: {e:?}");
        e
    })?;
    let state = clientlib::create_publisher(comms.as_ref(), PublisherDescriptors::<WifiState>::WIFI_STATE).await.map_err(|e| {
        error!("Failed to create publisher WifiState#wifi-state: {e:?}");
        e
    })?;
    info!("serving WifiService on the channel wifi");
    wifi_loop(networks, state, requests_rx).await;
    Ok(())
}

/// What the service asks the session for, and where the answer goes.
enum Request {
    Scan(oneshot::Sender<WifiNetworks>),
    Connect(WifiConnect, oneshot::Sender<WifiResult>),
    Forget(WifiForget, oneshot::Sender<WifiResult>),
    Disconnect(oneshot::Sender<WifiResult>),
}

/// The WifiService server: every call becomes a request to the session and waits for its
/// answer (a connect: until the network is joined or the attempt failed).
struct WifiServiceRpc {
    requests: mpsc::Sender<Request>,
}

impl WifiServiceRpc {
    async fn ask<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Request, wait: Duration) -> SdvResult<T> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.requests.send(make(reply_tx)).await.map_err(|_| SdvStatus::with_message(SdvStatusCode::Unavailable, "the Wi-Fi session is gone"))?;
        match timeout(wait, reply_rx).await {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(_)) => Err(SdvStatus::with_message(SdvStatusCode::Unavailable, "the supplicant connection was lost")),
            Err(_) => Err(SdvStatus::with_message(SdvStatusCode::DeadlineExceeded, "no answer from the supplicant in time")),
        }
    }
}

#[async_trait::async_trait]
#[allow(non_snake_case)]
impl com_everyware_wifi_service_rpc::Interface for WifiServiceRpc {
    async fn Scan(&self, _caller_id: sdv::comms::id::ServiceFqin, _request: &WifiScan) -> SdvResult<WifiNetworks> {
        self.ask(Request::Scan, SCAN_TIMEOUT).await
    }

    async fn Connect(&self, _caller_id: sdv::comms::id::ServiceFqin, request: &WifiConnect) -> SdvResult<WifiResult> {
        let request = request.clone();
        self.ask(move |reply| Request::Connect(request, reply), CONNECT_TIMEOUT).await
    }

    async fn Forget(&self, _caller_id: sdv::comms::id::ServiceFqin, request: &WifiForget) -> SdvResult<WifiResult> {
        let request = request.clone();
        self.ask(move |reply| Request::Forget(request, reply), SCAN_TIMEOUT).await
    }

    async fn Disconnect(&self, _caller_id: sdv::comms::id::ServiceFqin, _request: &WifiDisconnect) -> SdvResult<WifiResult> {
        self.ask(Request::Disconnect, SCAN_TIMEOUT).await
    }
}

fn result(ok: bool, error: &str) -> WifiResult {
    let mut message = WifiResult::new();
    message.ok = ok;
    message.error = error.to_string();
    message
}

/// Connects to the supplicant (again, whenever the connection is lost) and runs a session.
async fn wifi_loop(networks: Publisher<WifiNetworks>, state: Publisher<WifiState>, mut requests: mpsc::Receiver<Request>) {
    let (mut networks, mut state) = (networks, state);
    loop {
        let opened = (|| -> io::Result<(Ctrl, Ctrl)> { Ok((Ctrl::open(GLOBAL_CTRL, IFACE, "req")?, Ctrl::open(GLOBAL_CTRL, IFACE, "events")?)) })();
        let (ctrl, events) = match opened {
            Ok(pair) => pair,
            Err(err) => {
                debug!("no supplicant at {GLOBAL_CTRL} yet: {err}");
                sleep(RETRY_INTERVAL).await;
                continue;
            }
        };
        // the session owns the publishers while it runs (a publisher is not shared)
        let mut session = Session {
            ctrl,
            events,
            networks,
            state,
            scan: Vec::new(),
            attempt: None,
            scan_replies: Vec::new(),
            last_networks: None,
            last_state: None,
        };
        let result = session.run(&mut requests).await;
        if let Err(err) = &result {
            warn!("the supplicant session ended: {err}; connecting again");
            session.publish_state(WifiConnection::WIFI_DISCONNECTED, "", 0, "");
        }
        (networks, state) = (session.networks, session.state);
        if result.is_ok() {
            return;
        }
        sleep(RETRY_INTERVAL).await;
    }
}

/// A connect in progress: the supplicant's network id, the SSID, whether this attempt added
/// the network (a failed new one is removed again, as Android does) and who waits for the
/// outcome. Events of other networks (a late CONNECTED of the one left) do not end it.
struct Attempt {
    id: u32,
    ssid: String,
    added: bool,
    reply: Option<oneshot::Sender<WifiResult>>,
}

/// A network of the last scan, the strongest BSS of each SSID.
struct Seen {
    ssid: String,
    signal_dbm: i32,
    security: WifiSecurity,
}

/// One connection to the supplicant: the requests, the events, what was seen last.
struct Session {
    ctrl: Ctrl,
    events: Ctrl,
    networks: Publisher<WifiNetworks>,
    state: Publisher<WifiState>,
    scan: Vec<Seen>,
    attempt: Option<Attempt>,
    /// Scan calls waiting for the next results.
    scan_replies: Vec<oneshot::Sender<WifiNetworks>>,
    /// What was published last: the supplicant repeats its events (a rekey is a
    /// CTRL-EVENT-CONNECTED), the topics do not.
    last_networks: Option<WifiNetworks>,
    last_state: Option<WifiState>,
}

impl Session {
    /// Events, requests and the scan timer, until the supplicant stops answering (Err) or
    /// the requests end (Ok).
    async fn run(&mut self, requests: &mut mpsc::Receiver<Request>) -> io::Result<()> {
        self.events.attach().await?;
        info!("connected to the supplicant at {GLOBAL_CTRL}");
        // SAVE_CONFIG writes the supplicant's config only with this set (the config the
        // bring-up script writes may not say so).
        if let Err(err) = self.ctrl.ok("SET update_config 1").await {
            warn!("networks will not be kept: {err}");
        }
        // the list shows what the last scan saw, not what the cache still holds
        for command in [format!("BSS_EXPIRE_COUNT {BSS_EXPIRE_COUNT}"), format!("BSS_EXPIRE_AGE {}", BSS_EXPIRE_AGE.as_secs())] {
            if let Err(err) = self.ctrl.ok(&command).await {
                warn!("{command}: {err}");
            }
        }
        self.publish_status("").await?;
        self.scan().await?;
        let mut tick = interval(SCAN_INTERVAL);
        tick.tick().await;
        loop {
            tokio::select! {
                event = self.events.event() => self.on_event(&event?).await?,
                request = requests.recv() => match request {
                    Some(request) => self.on_request(request).await?,
                    None => return Ok(()),
                },
                _ = tick.tick() => self.scan().await?,
            }
        }
    }

    async fn scan(&mut self) -> io::Result<()> {
        match self.ctrl.request("SCAN").await?.as_str() {
            "OK" | "FAIL-BUSY" => Ok(()),
            other => Err(io::Error::other(format!("SCAN: {other}"))),
        }
    }

    /// The supplicant's events (hostap ctrl_iface events).
    async fn on_event(&mut self, event: &str) -> io::Result<()> {
        let kind = event.split_whitespace().next().unwrap_or("");
        match kind {
            "CTRL-EVENT-SCAN-RESULTS" => {
                let reply = self.ctrl.request("SCAN_RESULTS").await?;
                let mut best: HashMap<String, Seen> = HashMap::new();
                for entry in parse_scan_results(&reply) {
                    let security = security_of(&entry.flags);
                    match best.get_mut(&entry.ssid) {
                        Some(seen) if seen.signal_dbm >= entry.signal_dbm => {}
                        Some(seen) => {
                            seen.signal_dbm = entry.signal_dbm;
                            seen.security = security;
                        }
                        None => {
                            best.insert(entry.ssid.clone(), Seen { ssid: entry.ssid, signal_dbm: entry.signal_dbm, security });
                        }
                    }
                }
                self.scan = best.into_values().collect();
                self.scan.sort_by(|a, b| b.signal_dbm.cmp(&a.signal_dbm));
                let message = self.publish_networks().await?;
                for reply in self.scan_replies.drain(..) {
                    let _ = reply.send(message.clone());
                }
            }
            "CTRL-EVENT-CONNECTED" => {
                // "Connection to <bssid> completed [id=N id_str=]": the attempt is over only
                // when it is this network that connected
                if self.attempt.as_ref().is_some_and(|a| event_id(event) == Some(a.id)) {
                    let attempt = self.attempt.take().expect("checked");
                    info!("joined {:?}", attempt.ssid);
                    // SELECT_NETWORK left the other networks disabled (and SAVE_CONFIG would
                    // keep them so); known ones are candidates again, now and at the next boot
                    self.ctrl.ok("ENABLE_NETWORK all").await?;
                    if let Err(err) = self.ctrl.ok("SAVE_CONFIG").await {
                        warn!("the config is not written: {err}");
                    }
                    if let Some(reply) = attempt.reply {
                        let _ = reply.send(result(true, ""));
                    }
                }
                self.publish_status("").await?;
                self.publish_networks().await?;
            }
            "CTRL-EVENT-DISCONNECTED" => {
                if self.attempt.is_none() {
                    self.publish_status("").await?;
                    self.publish_networks().await?;
                }
            }
            "CTRL-EVENT-SSID-TEMP-DISABLED" => {
                // wrong password: the supplicant gives up on the network for a while
                let reason = event.split_whitespace().find_map(|f| f.strip_prefix("reason=")).unwrap_or("");
                if self.attempt.as_ref().is_some_and(|a| event_id(event) == Some(a.id)) {
                    let error = if reason == "WRONG_KEY" { "wrong password" } else { "authentication failed" };
                    self.fail_attempt(reason, error).await?;
                }
            }
            "CTRL-EVENT-NETWORK-NOT-FOUND" => {
                if self.attempt.is_some() {
                    self.fail_attempt("not found", "network not found").await?;
                }
            }
            _ => debug!("event {event}"),
        }
        Ok(())
    }

    /// The service's requests.
    async fn on_request(&mut self, request: Request) -> io::Result<()> {
        match request {
            Request::Scan(reply) => {
                self.scan_replies.push(reply);
                self.scan().await
            }
            Request::Connect(connect, reply) => {
                info!("connect to {:?}", connect.ssid);
                if let Some(previous) = self.attempt.take() {
                    if let Some(previous_reply) = previous.reply {
                        let _ = previous_reply.send(result(false, "another connect started"));
                    }
                }
                match self.connect(&connect).await {
                    Ok((id, added)) => {
                        self.attempt = Some(Attempt { id, ssid: connect.ssid.clone(), added, reply: Some(reply) });
                        self.publish_state(WifiConnection::WIFI_CONNECTING, &connect.ssid, 0, "");
                    }
                    Err(err) => {
                        warn!("connect to {:?}: {err}", connect.ssid);
                        self.publish_state(WifiConnection::WIFI_FAILED, &connect.ssid, 0, &err.to_string());
                        let _ = reply.send(result(false, &err.to_string()));
                    }
                }
                Ok(())
            }
            Request::Forget(forget, reply) => {
                info!("forget {:?}", forget.ssid);
                if let Some(id) = self.saved_id(&forget.ssid).await? {
                    self.ctrl.ok(&format!("REMOVE_NETWORK {id}")).await?;
                    self.ctrl.ok("SAVE_CONFIG").await?;
                }
                self.publish_status("").await?;
                self.publish_networks().await?;
                let _ = reply.send(result(true, ""));
                Ok(())
            }
            Request::Disconnect(reply) => {
                info!("disconnect");
                self.ctrl.ok("DISCONNECT").await?;
                let _ = reply.send(result(true, ""));
                Ok(())
            }
        }
    }

    /// The attempt failed: a network added for it is removed again (a wrong password is
    /// not kept), the known networks are candidates again, the caller and the screen learn
    /// why.
    async fn fail_attempt(&mut self, reason: &str, error: &str) -> io::Result<()> {
        let Some(attempt) = self.attempt.take() else { return Ok(()) };
        warn!("connect to {:?} failed: {reason}", attempt.ssid);
        if attempt.added {
            self.ctrl.ok(&format!("REMOVE_NETWORK {}", attempt.id)).await?;
        }
        self.ctrl.ok("ENABLE_NETWORK all").await?;
        if let Err(err) = self.ctrl.ok("SAVE_CONFIG").await {
            warn!("the config is not written: {err}");
        }
        self.publish_state(WifiConnection::WIFI_FAILED, &attempt.ssid, 0, error);
        if let Some(reply) = attempt.reply {
            let _ = reply.send(result(false, error));
        }
        Ok(())
    }

    /// The saved network with this SSID.
    async fn saved_id(&mut self, ssid: &str) -> io::Result<Option<u32>> {
        let reply = self.ctrl.request("LIST_NETWORKS").await?;
        Ok(parse_networks(&reply).into_iter().find(|(_, s, _)| s == ssid).map(|(id, _, _)| id))
    }

    /// Joins a network: a saved one as it is, a new one with its password, and keeps it
    /// (SAVE_CONFIG: the supplicant's config holds the password, not this bundle). Returns
    /// the supplicant's id of the network and whether it was added now.
    async fn connect(&mut self, connect: &WifiConnect) -> io::Result<(u32, bool)> {
        let security = connect.security.enum_value_or_default();
        if security == WifiSecurity::WIFI_SECURITY_ENTERPRISE {
            return Err(io::Error::other("enterprise networks need a certificate"));
        }
        let (id, added) = match self.saved_id(&connect.ssid).await? {
            Some(id) => (id, false),
            None => (self.ctrl.request("ADD_NETWORK").await?.parse().map_err(|_| io::Error::other("ADD_NETWORK failed"))?, true),
        };
        self.ctrl.ok(&format!("SET_NETWORK {id} ssid \"{}\"", connect.ssid)).await?;
        match security {
            WifiSecurity::WIFI_SECURITY_OPEN => self.ctrl.ok(&format!("SET_NETWORK {id} key_mgmt NONE")).await?,
            WifiSecurity::WIFI_SECURITY_WPA3 => {
                self.ctrl.ok(&format!("SET_NETWORK {id} key_mgmt SAE")).await?;
                self.ctrl.ok(&format!("SET_NETWORK {id} ieee80211w 2")).await?;
            }
            _ => {}
        }
        if !connect.psk.is_empty() {
            self.ctrl.ok(&format!("SET_NETWORK {id} psk \"{}\"", connect.psk)).await?;
        }
        self.ctrl.ok(&format!("ENABLE_NETWORK {id}")).await?;
        self.ctrl.ok(&format!("SELECT_NETWORK {id}")).await?;
        if let Err(err) = self.ctrl.ok("SAVE_CONFIG").await {
            warn!("the network is not kept: {err}");
        }
        Ok((id, added))
    }

    /// The state from STATUS and SIGNAL_POLL.
    async fn publish_status(&mut self, error: &str) -> io::Result<()> {
        let status = self.ctrl.request(STATUS).await?;
        let ssid = status_value(&status, "ssid").unwrap_or("").to_string();
        let connection = match status_value(&status, "wpa_state").unwrap_or("") {
            "COMPLETED" => WifiConnection::WIFI_CONNECTED,
            "SCANNING" => WifiConnection::WIFI_SCANNING,
            "ASSOCIATING" | "ASSOCIATED" | "AUTHENTICATING" | "4WAY_HANDSHAKE" | "GROUP_HANDSHAKE" => WifiConnection::WIFI_CONNECTING,
            _ => WifiConnection::WIFI_DISCONNECTED,
        };
        let signal = if connection == WifiConnection::WIFI_CONNECTED {
            let poll = self.ctrl.request("SIGNAL_POLL").await.unwrap_or_default();
            status_value(&poll, "RSSI").and_then(|v| v.parse().ok()).map_or(0, bars)
        } else {
            0
        };
        self.publish_state(connection, &ssid, signal, error);
        Ok(())
    }

    // Not async: a shared borrow of the session across an await would need it Sync.
    fn publish_state(&mut self, connection: WifiConnection, ssid: &str, signal_bars: u32, error: &str) {
        let mut message = WifiState::new();
        message.connection = connection.into();
        message.ssid = ssid.to_string();
        message.signal_bars = signal_bars;
        message.error = error.to_string();
        // the same again, or only the signal flickering by a bar: nothing new
        if self.last_state.as_ref().is_some_and(|last| {
            last.connection == message.connection
                && last.ssid == message.ssid
                && last.error == message.error
                && last.signal_bars.abs_diff(message.signal_bars) <= 1
        }) {
            return;
        }
        info!("state {connection:?} {ssid:?} {signal_bars} bars {error:?}");
        if let Err(e) = self.state.publish(&message) {
            error!("Failed to publish on WifiState#wifi-state: {e:?}");
        }
        self.last_state = Some(message);
    }

    /// The last scan, with what is saved and what the device is on; returns the message.
    async fn publish_networks(&mut self) -> io::Result<WifiNetworks> {
        let saved = parse_networks(&self.ctrl.request("LIST_NETWORKS").await?);
        let current = saved.iter().find(|(_, _, current)| *current).map(|(_, ssid, _)| ssid.clone());
        let status = self.ctrl.request(STATUS).await?;
        let connected = (status_value(&status, "wpa_state") == Some("COMPLETED")).then_some(current).flatten();
        let mut message = WifiNetworks::new();
        for seen in self.scan.iter().take(32) {
            let mut network = WifiNetwork::new();
            network.ssid = seen.ssid.clone();
            network.security = seen.security.into();
            network.signal_dbm = seen.signal_dbm;
            network.signal_bars = bars(seen.signal_dbm);
            network.saved = saved.iter().any(|(_, ssid, _)| *ssid == seen.ssid);
            network.connected = connected.as_deref() == Some(seen.ssid.as_str());
            message.networks.push(network);
        }
        if self.last_networks.as_ref() != Some(&message) {
            info!("{} networks around, on {:?}", message.networks.len(), connected);
            if let Err(e) = self.networks.publish(&message) {
                error!("Failed to publish on WifiNetworks#wifi-networks: {e:?}");
            }
            self.last_networks = Some(message.clone());
        }
        Ok(message)
    }
}

/// The network id of an event (`... [id=1 id_str=]`, `... id=1 ssid="x" ...`).
fn event_id(event: &str) -> Option<u32> {
    event.split(|c: char| c.is_whitespace() || c == '[' || c == ']').find_map(|f| f.strip_prefix("id=")).and_then(|v| v.parse().ok())
}

/// The security from the BSS flags (`[WPA2-PSK-CCMP][SAE][ESS]`).
fn security_of(flags: &str) -> WifiSecurity {
    if flags.contains("EAP") {
        WifiSecurity::WIFI_SECURITY_ENTERPRISE
    } else if flags.contains("SAE") {
        WifiSecurity::WIFI_SECURITY_WPA3
    } else if flags.contains("PSK") {
        WifiSecurity::WIFI_SECURITY_WPA2
    } else {
        WifiSecurity::WIFI_SECURITY_OPEN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_of_an_event() {
        assert_eq!(event_id("CTRL-EVENT-CONNECTED - Connection to 24:99:00:5f:d5:31 completed [id=1 id_str=]"), Some(1));
        assert_eq!(event_id("CTRL-EVENT-SSID-TEMP-DISABLED id=3 ssid=\"x\" auth_failures=1 duration=10 reason=WRONG_KEY"), Some(3));
        assert_eq!(event_id("CTRL-EVENT-DISCONNECTED bssid=aa reason=15"), None);
    }

    #[test]
    fn security_from_the_flags() {
        assert_eq!(security_of("[WPA2-PSK-CCMP][ESS]"), WifiSecurity::WIFI_SECURITY_WPA2);
        assert_eq!(security_of("[WPA2-PSK-CCMP][SAE][ESS]"), WifiSecurity::WIFI_SECURITY_WPA3);
        assert_eq!(security_of("[WPA2-EAP-CCMP][ESS]"), WifiSecurity::WIFI_SECURITY_ENTERPRISE);
        assert_eq!(security_of("[ESS]"), WifiSecurity::WIFI_SECURITY_OPEN);
    }
}
