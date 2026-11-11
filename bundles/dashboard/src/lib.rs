//! DashboardServiceBundle: the bridge between the house's topics and the screen. It
//! subscribes to what the dashboard shows (`weather`, `news`, `news-thumbnails`,
//! `wifi-networks`, `wifi-state`, `matter-devices`, catalog/everyware.vsidl) and pushes every
//! update to the panel over binder (screen.rs, interfaces/screen IScreenLink); the panel itself
//! is no SDV participant. The same shape as HAR's HarSdvServiceBundle pushing to harry-app: the
//! screen serves, the bundle calls it. The other way, the screen's commands come into this
//! bundle's sink: a Wi-Fi command (it may carry the password typed on the screen) becomes a call
//! of the Wi-Fi bundle's `WifiService`, a Matter command one of the Matter bundle's
//! `MatterService` (SDV RPC, catalog/*_service.proto), and the answer goes back to the screen.
//!
//! The bundle keeps the last value of every topic and sends it again when the panel comes back
//! (it restarted and starts from nothing). Whether the link is up, the panel learns from binder:
//! it watches the bundle's sink (a death recipient), so there is no heartbeat.
//!
//! The service bundle itself is shaped as vsidlc generates it (`vsidlc --services`): a
//! `ServiceBundle` with `SdvComms`, a current-thread runtime, `sdv_main` with the bundle's
//! subscriptions; the business logic is in the `handle_*` functions.

use futures_util::stream::StreamExt;
use log::{debug, error, info, warn};
use protobuf::Message;
use sdv::mw::{clientlib, Availability, Communicate, SdvComms, SubscribeOptions};
use sdv::status::SdvResult;
use com_everyware_matter_service_rpc::Client as MatterClient;
use com_everyware_wifi_service_rpc::Client as WifiClient;
use everyware_catalog::matter_service::matter_command::Command as MatterCmd;
use everyware_catalog::wifi::wifi_command::Command;
use sdv_mw_rs_com_everyware_dashboard_service_bundle::{ClientDescriptors, DashboardServiceBundle as Bundle, SubscriberDescriptors};
use std::fmt::Debug;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

mod screen;
use screen::{Screen, ScreenCommand};

/// How often the panel is looked for while it is not there, and the sink handed again while it
/// does not take it.
const TICK: Duration = Duration::from_secs(1);
/// The deadline of a WifiService call: a connect is answered when the network is joined or
/// the supplicant gave up on it (the Wi-Fi bundle answers within 35 s).
const WIFI_CALL_TIMEOUT: Duration = Duration::from_secs(40);
/// The deadline of a MatterService call: a pairing is answered within the Matter bundle's
/// 150 s, a device command within its 20 s.
const MATTER_CALL_TIMEOUT: Duration = Duration::from_secs(160);

type News = everyware_catalog::news::NewsFeed;
type Thumbnail = everyware_catalog::news::NewsThumbnail;
type Thumbnails = everyware_catalog::news::NewsThumbnails;
type Weather = everyware_catalog::weather::Weather;
type WifiNetworks = everyware_catalog::wifi::WifiNetworks;
type WifiState = everyware_catalog::wifi::WifiState;
type WifiCommand = everyware_catalog::wifi::WifiCommand;
type WifiResult = everyware_catalog::wifi_service::WifiResult;
type MatterDevices = everyware_catalog::matter::MatterDevices;
type MatterCommand = everyware_catalog::matter_service::MatterCommand;
type MatterResult = everyware_catalog::matter_service::MatterResult;

/// The last value of every topic, sent again when the panel comes back.
#[derive(Default)]
struct Last {
    weather: Option<Weather>,
    news: Option<News>,
    /// The thumbnails of the stories of the last feed, by id.
    thumbnails: std::collections::BTreeMap<u64, Thumbnail>,
    wifi_networks: Option<WifiNetworks>,
    wifi_state: Option<WifiState>,
    matter_devices: Option<MatterDevices>,
}

/// The services the screen's commands go to, once they are there (the bundles start in any
/// order).
#[derive(Default)]
struct Services {
    wifi: OnceLock<Arc<WifiClient>>,
    matter: OnceLock<Arc<MatterClient>>,
}

sdv::lifecycle::register_service_bundle!(DashboardServiceBundle);

struct DashboardServiceBundle {
    cancellation_token: Option<CancellationToken>,
    context: ContextRef,
}

impl ServiceBundle for DashboardServiceBundle {
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
                    () = cancellation_token.cancelled() => Err(sdv::status::SdvStatus::new(sdv::status::SdvStatusCode::Cancelled)),
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

/// Waits until a publisher registered the topic (as the generated bundles do).
async fn wait_for_publisher_to_be_available<Proto: protobuf::Message + Debug>(
    comms: &dyn Communicate,
    descriptor: &clientlib::SubscriberDescriptor<Proto>,
) -> SdvResult<()> {
    let mut event_stream = clientlib::create_registration_event_stream(comms, descriptor).await.map_err(|e| {
        error!("unable to generate a registration event: {e:?}");
        e
    })?;
    loop {
        match event_stream.next().await {
            Some(Availability::Available) => break,
            Some(Availability::Unavailable) => debug!("Still waiting for publisher..."),
            None => break,
        }
    }
    Ok(())
}

async fn sdv_main(comms: Arc<dyn Communicate>) -> SdvResult<()> {
    info!("Starting service bundle 'DashboardServiceBundle'");
    let _service_bundle = match Bundle::builder().set_comms(comms.clone()).build().await {
        Ok(service) => service,
        Err(e) => {
            error!("Failed to build service bundle: {e}");
            return Err(sdv::status::SdvStatus::with_message(sdv::status::SdvStatusCode::Internal, e.to_string()));
        }
    };

    let (commands_tx, commands_rx) = unbounded_channel();
    let (deaths_tx, deaths_rx) = unbounded_channel();
    let screen = Arc::new(Screen::new(commands_tx, deaths_tx));
    let last = Arc::new(Mutex::new(Last::default()));
    let services = Arc::new(Services::default());

    let mut futures_join_set = tokio::task::JoinSet::new();
    futures_join_set.spawn(handle_screen(screen.clone(), last.clone(), deaths_rx));
    futures_join_set.spawn(handle_commands(commands_rx, screen.clone(), services.clone()));
    // The services the commands go to, each on its own: a bundle may come late or not at all.
    // Not in the join set: they end once their service is found.
    tokio::spawn(find_wifi(comms.clone(), services.clone()));
    // The Matter devices on their own too: the subscriptions below wait for their publishers
    // one after the other.
    tokio::spawn(matter_link(comms.clone(), screen.clone(), last.clone(), services.clone()));
    // The publishers may start later than this bundle: every subscription waits on its own.
    // Every topic is state (the weather now, the feed now, the networks now): a subscription
    // gets the publication's last message at once (read_on_subscribe), so what a bundle
    // published before this one subscribed, its last reading kept from the previous start
    // among it, reaches the screen (the thumbnails of a feed are one message for this).
    let latest = SubscribeOptions::builder().read_on_subscribe(true).build()?;
    let weather = SubscriberDescriptors::<Weather>::WEATHER;
    if wait_for_publisher_to_be_available(comms.as_ref(), weather).await.is_ok() {
        match clientlib::create_observer(comms.as_ref(), weather, &latest).await {
            Ok(observer) => {
                futures_join_set.spawn(handle_weather_observer(observer, screen.clone(), last.clone()));
            }
            Err(e) => error!("Failed to create observer Weather#WEATHER: {e:?}"),
        }
    }
    let networks = SubscriberDescriptors::<WifiNetworks>::WIFI_NETWORKS;
    if wait_for_publisher_to_be_available(comms.as_ref(), networks).await.is_ok() {
        match clientlib::create_observer(comms.as_ref(), networks, &latest).await {
            Ok(observer) => {
                futures_join_set.spawn(handle_wifi_networks_observer(observer, screen.clone(), last.clone()));
            }
            Err(e) => error!("Failed to create observer WifiNetworks#wifi-networks: {e:?}"),
        }
    }
    let state = SubscriberDescriptors::<WifiState>::WIFI_STATE;
    if wait_for_publisher_to_be_available(comms.as_ref(), state).await.is_ok() {
        match clientlib::create_observer(comms.as_ref(), state, &latest).await {
            Ok(observer) => {
                futures_join_set.spawn(handle_wifi_state_observer(observer, screen.clone(), last.clone()));
            }
            Err(e) => error!("Failed to create observer WifiState#wifi-state: {e:?}"),
        }
    }
    let news = SubscriberDescriptors::<News>::NEWS;
    if wait_for_publisher_to_be_available(comms.as_ref(), news).await.is_ok() {
        match clientlib::create_observer(comms.as_ref(), news, &latest).await {
            Ok(observer) => {
                futures_join_set.spawn(handle_news_observer(observer, screen.clone(), last.clone()));
            }
            Err(e) => error!("Failed to create observer NewsFeed#news: {e:?}"),
        }
    }
    let thumbnails = SubscriberDescriptors::<Thumbnails>::NEWS_THUMBNAILS;
    if wait_for_publisher_to_be_available(comms.as_ref(), thumbnails).await.is_ok() {
        match clientlib::create_observer(comms.as_ref(), thumbnails, &latest).await {
            Ok(observer) => {
                futures_join_set.spawn(handle_thumbnail_observer(observer, screen.clone(), last.clone()));
            }
            Err(e) => error!("Failed to create observer NewsThumbnails#news-thumbnails: {e:?}"),
        }
    }
    while let Some(e) = futures_join_set.join_next().await {
        error!("Future joined with result: {e:?}");
    }
    Ok(())
}

type Observer<T> = std::pin::Pin<Box<dyn futures::Stream<Item = SdvResult<Vec<T>>> + Send>>;

/// Every message of the weather topic goes to the screen; the last one is kept.
async fn handle_weather_observer(mut observer: Observer<Weather>, screen: Arc<Screen>, last: Arc<Mutex<Last>>) {
    info!("subscribed to Weather#weather");
    while let Some(message) = observer.next().await {
        match message {
            Ok(messages) => {
                for weather in messages {
                    info!("weather {} °C at {}, {} hours", weather.temperature_c, weather.time, weather.hourly.len());
                    screen.publish_weather(&weather);
                    last.lock().expect("last values").weather = Some(weather);
                }
            }
            Err(e) => error!("Error receiving message on Weather#WEATHER: {e:?}"),
        }
    }
    warn!("the weather stream ended");
}

/// Every feed of the news topic goes to the screen; the last one is kept.
async fn handle_news_observer(mut observer: Observer<News>, screen: Arc<Screen>, last: Arc<Mutex<Last>>) {
    info!("subscribed to NewsFeed#news");
    while let Some(message) = observer.next().await {
        match message {
            Ok(messages) => {
                for news in messages {
                    info!("{} news stories", news.stories.len());
                    screen.publish_news(&news);
                    let mut last = last.lock().expect("last values");
                    // the thumbnails of stories that left the feed go
                    last.thumbnails.retain(|id, _| news.stories.iter().any(|s| s.id == *id));
                    last.news = Some(news);
                }
            }
            Err(e) => error!("Error receiving message on NewsFeed#news: {e:?}"),
        }
    }
    warn!("the news stream ended");
}

/// Every thumbnail goes to the screen; the ones of the current feed are kept.
async fn handle_thumbnail_observer(mut observer: Observer<Thumbnails>, screen: Arc<Screen>, last: Arc<Mutex<Last>>) {
    info!("subscribed to NewsThumbnails#news-thumbnails");
    while let Some(message) = observer.next().await {
        match message {
            Ok(messages) => {
                for thumbnail in messages.into_iter().flat_map(|m| m.thumbnails) {
                    debug!("thumbnail of story {}", thumbnail.id);
                    screen.publish_thumbnail(&thumbnail);
                    last.lock().expect("last values").thumbnails.insert(thumbnail.id, thumbnail);
                }
            }
            Err(e) => error!("Error receiving message on NewsThumbnails#news-thumbnails: {e:?}"),
        }
    }
    warn!("the thumbnail stream ended");
}

/// Every message of the networks topic goes to the screen; the last one is kept.
async fn handle_wifi_networks_observer(mut observer: Observer<WifiNetworks>, screen: Arc<Screen>, last: Arc<Mutex<Last>>) {
    info!("subscribed to WifiNetworks#wifi-networks");
    while let Some(message) = observer.next().await {
        match message {
            Ok(messages) => {
                for message in messages {
                    info!("{} Wi-Fi networks", message.networks.len());
                    screen.publish_networks(&message);
                    last.lock().expect("last values").wifi_networks = Some(message);
                }
            }
            Err(e) => error!("Error receiving message on WifiNetworks#wifi-networks: {e:?}"),
        }
    }
    warn!("the Wi-Fi networks stream ended");
}

/// Every message of the state topic goes to the screen; the last one is kept.
async fn handle_wifi_state_observer(mut observer: Observer<WifiState>, screen: Arc<Screen>, last: Arc<Mutex<Last>>) {
    info!("subscribed to WifiState#wifi-state");
    while let Some(message) = observer.next().await {
        match message {
            Ok(messages) => {
                for message in messages {
                    info!("Wi-Fi {:?} {:?}", message.connection, message.ssid);
                    screen.publish_state(&message);
                    last.lock().expect("last values").wifi_state = Some(message);
                }
            }
            Err(e) => error!("Error receiving message on WifiState#wifi-state: {e:?}"),
        }
    }
    warn!("the Wi-Fi state stream ended");
}

/// The Matter devices to the screen, and the Matter bundle's service for the screen's commands,
/// once its publication and its service are there.
async fn matter_link(comms: Arc<dyn Communicate>, screen: Arc<Screen>, last: Arc<Mutex<Last>>, services: Arc<Services>) {
    let Ok(latest) = SubscribeOptions::builder().read_on_subscribe(true).build() else { return };
    let devices = SubscriberDescriptors::<MatterDevices>::MATTER_DEVICES;
    if wait_for_publisher_to_be_available(comms.as_ref(), devices).await.is_ok() {
        match clientlib::create_observer(comms.as_ref(), devices, &latest).await {
            Ok(observer) => {
                tokio::spawn(handle_matter_observer(observer, screen, last));
            }
            Err(e) => error!("Failed to create observer MatterDevices#matter-devices: {e:?}"),
        }
    }
    if let Some(matter) = create_matter_client(comms.as_ref()).await {
        let _ = services.matter.set(Arc::new(matter));
    }
}

/// Every message of the Matter topic goes to the screen; the last one is kept.
async fn handle_matter_observer(mut observer: Observer<MatterDevices>, screen: Arc<Screen>, last: Arc<Mutex<Last>>) {
    info!("subscribed to MatterDevices#matter-devices");
    while let Some(message) = observer.next().await {
        match message {
            Ok(messages) => {
                for devices in messages {
                    debug!("{} Matter nodes", devices.nodes.len());
                    screen.publish_matter_devices(&devices);
                    last.lock().expect("last values").matter_devices = Some(devices);
                }
            }
            Err(e) => error!("Error receiving message on MatterDevices#matter-devices: {e:?}"),
        }
    }
    warn!("the Matter devices stream ended");
}

/// The client of the Matter bundle's MatterService, once the server is registered.
async fn create_matter_client(comms: &dyn Communicate) -> Option<MatterClient> {
    let descriptor = ClientDescriptors::<MatterClient>::MATTER;
    let Ok(mut events) = clientlib::create_server_registration_event_stream(comms, descriptor).await else {
        error!("Failed to create the registration event stream of MatterService#matter");
        return None;
    };
    while let Some(event) = events.next().await {
        if event == Availability::Available {
            break;
        }
        debug!("waiting for MatterService#matter");
    }
    let options = sdv::mw::ClientOptions::builder().timeout_in_sec(MATTER_CALL_TIMEOUT.as_secs() as u32).build().ok()?;
    match clientlib::create_rpc_client(comms, descriptor, options).await {
        Ok(client) => {
            info!("calling MatterService#matter");
            Some(client)
        }
        Err(e) => {
            error!("Failed to create the client of MatterService#matter: {e:?}");
            None
        }
    }
}

/// The Wi-Fi bundle's WifiService for the screen's commands, once it is there.
async fn find_wifi(comms: Arc<dyn Communicate>, services: Arc<Services>) {
    if let Some(wifi) = create_wifi_client(comms.as_ref()).await {
        let _ = services.wifi.set(Arc::new(wifi));
    }
}

/// The client of the Wi-Fi bundle's WifiService, once the server is registered.
async fn create_wifi_client(comms: &dyn Communicate) -> Option<WifiClient> {
    let descriptor = ClientDescriptors::<WifiClient>::WIFI;
    let Ok(mut events) = clientlib::create_server_registration_event_stream(comms, descriptor).await else {
        error!("Failed to create the registration event stream of WifiService#wifi");
        return None;
    };
    while let Some(event) = events.next().await {
        if event == Availability::Available {
            break;
        }
        debug!("waiting for WifiService#wifi");
    }
    let options = sdv::mw::ClientOptions::builder().timeout_in_sec(WIFI_CALL_TIMEOUT.as_secs() as u32).build().ok()?;
    match clientlib::create_rpc_client(comms, descriptor, options).await {
        Ok(client) => {
            info!("calling WifiService#wifi");
            Some(client)
        }
        Err(e) => {
            error!("Failed to create the client of WifiService#wifi: {e:?}");
            None
        }
    }
}

/// The panel over binder: looked for every tick while it is not there, and handed the sink
/// until it takes it; once it took it (it started, restarted, or stopped refusing), it gets
/// every last value again. Its death comes as a message.
async fn handle_screen(screen: Arc<Screen>, last: Arc<Mutex<Last>>, mut deaths: UnboundedReceiver<u64>) {
    loop {
        if screen.connect() {
            resend(&screen, &last);
        }
        tokio::select! {
            Some(generation) = deaths.recv() => screen.on_death(generation),
            () = sleep(TICK) => {}
        }
    }
}

/// The last values to a panel that starts from nothing.
fn resend(screen: &Screen, last: &Mutex<Last>) {
    let (weather, news, thumbnails, networks, state, matter) = {
        let last = last.lock().expect("last values");
        (
            last.weather.clone(),
            last.news.clone(),
            last.thumbnails.values().cloned().collect::<Vec<_>>(),
            last.wifi_networks.clone(),
            last.wifi_state.clone(),
            last.matter_devices.clone(),
        )
    };
    if let Some(weather) = weather {
        screen.publish_weather(&weather);
    }
    if let Some(news) = news {
        screen.publish_news(&news);
    }
    for thumbnail in &thumbnails {
        screen.publish_thumbnail(thumbnail);
    }
    if let Some(networks) = networks {
        screen.publish_networks(&networks);
    }
    if let Some(state) = state {
        screen.publish_state(&state);
    }
    if let Some(matter) = matter {
        screen.publish_matter_devices(&matter);
    }
}

/// The screen's commands: the panel calls this bundle's sink (screen.rs) on a binder thread.
/// Every command is a call of the owning bundle's service in a task of its own (a connect waits
/// for its outcome; a scan meanwhile does not wait for it), and its answer goes back to the
/// screen.
async fn handle_commands(mut commands: UnboundedReceiver<ScreenCommand>, screen: Arc<Screen>, services: Arc<Services>) {
    while let Some(command) = commands.recv().await {
        match command {
            ScreenCommand::Wifi(bytes) => {
                let Ok(command) = WifiCommand::parse_from_bytes(&bytes) else {
                    warn!("a Wi-Fi command that does not parse");
                    continue;
                };
                tokio::spawn(call_wifi(screen.clone(), services.wifi.get().cloned(), command));
            }
            ScreenCommand::Matter(bytes) => {
                let Ok(command) = MatterCommand::parse_from_bytes(&bytes) else {
                    warn!("a Matter command that does not parse");
                    continue;
                };
                tokio::spawn(call_matter(screen.clone(), services.matter.get().cloned(), command));
            }
        }
    }
}

/// One Wi-Fi command: the call, then its answer to the screen (a call that did not get through
/// is an answer too: the screen must not wait for a connect that never started).
async fn call_wifi(screen: Arc<Screen>, wifi: Option<Arc<WifiClient>>, command: WifiCommand) {
    let (name, ssid) = match &command.command {
        Some(Command::Scan(_)) => ("scan", String::new()),
        Some(Command::Connect(c)) => ("connect", c.ssid.clone()),
        Some(Command::Forget(f)) => ("forget", f.ssid.clone()),
        Some(Command::Disconnect(_)) => ("disconnect", String::new()),
        Some(_) | None => return,
    };
    let Some(wifi) = wifi else {
        warn!("Wi-Fi {name} {ssid:?}: the Wi-Fi service is not there yet");
        screen.publish_wifi_result(name, &ssid, &failed("the Wi-Fi service is not there yet"));
        return;
    };
    let answer = match command.command {
        Some(Command::Scan(scan)) => wifi.Scan(&scan).await.map(|_| ok()),
        Some(Command::Connect(connect)) => wifi.Connect(&connect).await,
        Some(Command::Forget(forget)) => wifi.Forget(&forget).await,
        Some(Command::Disconnect(disconnect)) => wifi.Disconnect(&disconnect).await,
        Some(_) | None => return,
    };
    // the password is in the request only, never in the log
    let result = match answer {
        Ok(result) => {
            info!("Wi-Fi {name} {ssid:?}: ok {} {:?}", result.ok, result.error);
            result
        }
        Err(e) => {
            warn!("Wi-Fi {name} {ssid:?}: the call failed: {e:?}");
            failed("the Wi-Fi service did not answer")
        }
    };
    screen.publish_wifi_result(name, &ssid, &result);
}

fn ok() -> WifiResult {
    let mut result = WifiResult::new();
    result.ok = true;
    result
}

fn failed(error: &str) -> WifiResult {
    let mut result = WifiResult::new();
    result.error = error.to_string();
    result
}

/// One Matter command, then its answer to the screen (a call that did not get through too).
async fn call_matter(screen: Arc<Screen>, matter: Option<Arc<MatterClient>>, command: MatterCommand) {
    let (name, node_id, endpoint) = match &command.command {
        Some(MatterCmd::Pair(_)) => ("pair", 0, 0),
        Some(MatterCmd::SetOnOff(r)) => ("on-off", r.node_id, r.endpoint),
        Some(MatterCmd::MoveToLevel(r)) => ("level", r.node_id, r.endpoint),
        Some(MatterCmd::SetSetpoint(r)) => ("setpoint", r.node_id, r.endpoint),
        Some(MatterCmd::SetSystemMode(r)) => ("system-mode", r.node_id, r.endpoint),
        Some(MatterCmd::SetFanMode(r)) => ("fan-mode", r.node_id, r.endpoint),
        Some(_) | None => return,
    };
    let fail = |error: &str| {
        let mut result = MatterResult::new();
        result.error = error.to_string();
        result
    };
    let Some(matter) = matter else {
        warn!("Matter {name} node {node_id:#x} ep{endpoint}: the Matter service is not there yet");
        screen.publish_matter_result(name, node_id, endpoint, &fail("the Matter service is not there yet"));
        return;
    };
    let answer = match command.command {
        Some(MatterCmd::Pair(r)) => matter.Pair(&r).await,
        Some(MatterCmd::SetOnOff(r)) => matter.SetOnOff(&r).await,
        Some(MatterCmd::MoveToLevel(r)) => matter.MoveToLevel(&r).await,
        Some(MatterCmd::SetSetpoint(r)) => matter.SetSetpoint(&r).await,
        Some(MatterCmd::SetSystemMode(r)) => matter.SetSystemMode(&r).await,
        Some(MatterCmd::SetFanMode(r)) => matter.SetFanMode(&r).await,
        Some(_) | None => return,
    };
    let result = match answer {
        Ok(result) => {
            info!("Matter {name} node {node_id:#x} ep{endpoint}: ok {} {:?}", result.ok, result.error);
            result
        }
        Err(e) => {
            warn!("Matter {name} node {node_id:#x} ep{endpoint}: the call failed: {e:?}");
            fail("the Matter service did not answer")
        }
    };
    screen.publish_matter_result(name, node_id, endpoint, &result);
}
