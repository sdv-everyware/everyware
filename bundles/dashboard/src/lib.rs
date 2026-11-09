//! DashboardServiceBundle: the bridge between the house's topics and the screen. It
//! subscribes to what the dashboard shows (`weather`, `news`, `wifi-networks`, `wifi-state`,
//! catalog/everyware.vsidl) and pushes every update over a local gRPC connection to the harry
//! process, which hosts the server on 127.0.0.1 (proto/dashboard.proto); the harry process
//! itself is no SDV participant. The same shape as HAR's HarSdvServiceBundle pushing to
//! harry-app. The other way, it takes the screen's Wi-Fi commands from a server stream of
//! that connection, calls the Wi-Fi bundle's `WifiService` (SDV RPC,
//! catalog/wifi_service.proto) for each and hands the answer back (`PublishWifiResult`).
//!
//! The bundle keeps the last value of every topic and sends it again when the link comes
//! back (the harry process restarted), and a heartbeat once a second, so the screen can tell
//! "no data" from "no change".
//!
//! The service bundle itself is shaped as vsidlc generates it (`vsidlc --services`): a
//! `ServiceBundle` with `SdvComms`, a current-thread runtime, `sdv_main` with the bundle's
//! subscriptions; the business logic is in the `handle_*` functions.

use futures_util::stream::StreamExt;
use grpcio::{ChannelBuilder, EnvBuilder};
use log::{debug, error, info, warn};
use protobuf::Message;
use sdv::mw::{clientlib, Availability, Communicate, SdvComms, SubscribeOptions};
use sdv::status::SdvResult;
use com_everyware_matter_service_rpc::Client as MatterClient;
use com_everyware_wifi_service_rpc::Client as WifiClient;
use everyware_catalog::matter_service::matter_command::Command as MatterCmd;
use everyware_catalog::wifi::wifi_command::Command;
use sdv_mw_rs_com_everyware_dashboard_service_bundle::{ClientDescriptors, DashboardServiceBundle as Bundle, SubscriberDescriptors};
use everyware_dashboard_grpc::dashboard::{HeartbeatRequest, MatterCommandResult, Subscribe, WifiCommandResult};
use everyware_dashboard_grpc::dashboard_grpc::DashboardServiceClient;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

/// Where the harry process listens (smarthome-app src/house_link.rs).
pub const HARRY_ADDRESS: &str = "127.0.0.1:7100";
/// Between heartbeats.
const HEARTBEAT: Duration = Duration::from_secs(1);
/// The deadline of a WifiService call: a connect is answered when the network is joined or
/// the supplicant gave up on it (the Wi-Fi bundle answers within 35 s).
const WIFI_CALL_TIMEOUT: Duration = Duration::from_secs(40);
/// The deadline of a MatterService call: a pairing is answered within the Matter bundle's
/// 150 s, a device command within its 20 s.
const MATTER_CALL_TIMEOUT: Duration = Duration::from_secs(160);

type GrpcWeather = everyware_dashboard_grpc::weather::Weather;
type GrpcNews = everyware_dashboard_grpc::news::NewsFeed;
type CatalogNews = everyware_catalog::news::NewsFeed;
type GrpcThumbnail = everyware_dashboard_grpc::news::NewsThumbnail;
type CatalogThumbnail = everyware_catalog::news::NewsThumbnail;
type CatalogThumbnails = everyware_catalog::news::NewsThumbnails;
type CatalogWeather = everyware_catalog::weather::Weather;
type GrpcWifiNetworks = everyware_dashboard_grpc::wifi::WifiNetworks;
type CatalogWifiNetworks = everyware_catalog::wifi::WifiNetworks;
type GrpcWifiState = everyware_dashboard_grpc::wifi::WifiState;
type CatalogWifiState = everyware_catalog::wifi::WifiState;
type GrpcWifiCommand = everyware_dashboard_grpc::wifi::WifiCommand;
type CatalogWifiCommand = everyware_catalog::wifi::WifiCommand;
type GrpcWifiResult = everyware_dashboard_grpc::wifi_service::WifiResult;
type CatalogWifiResult = everyware_catalog::wifi_service::WifiResult;
type GrpcMatterDevices = everyware_dashboard_grpc::matter::MatterDevices;
type CatalogMatterDevices = everyware_catalog::matter::MatterDevices;
type GrpcMatterCommand = everyware_dashboard_grpc::matter_service::MatterCommand;
type CatalogMatterCommand = everyware_catalog::matter_service::MatterCommand;
type GrpcMatterResult = everyware_dashboard_grpc::matter_service::MatterResult;
type CatalogMatterResult = everyware_catalog::matter_service::MatterResult;

/// The last value of every topic, sent again after a reconnect.
#[derive(Default)]
struct Last {
    weather: Option<GrpcWeather>,
    news: Option<GrpcNews>,
    /// The thumbnails of the stories of the last feed, by id.
    thumbnails: std::collections::BTreeMap<u64, GrpcThumbnail>,
    wifi_networks: Option<GrpcWifiNetworks>,
    wifi_state: Option<GrpcWifiState>,
    matter_devices: Option<GrpcMatterDevices>,
}

/// The catalog's type and the gRPC crate's are the same wire format.
fn convert<From: Message, To: Message>(message: &From) -> Option<To> {
    To::parse_from_bytes(&message.write_to_bytes().ok()?).ok()
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

    // One channel to the harry process; grpcio reconnects it on its own.
    let channel = ChannelBuilder::new(Arc::new(EnvBuilder::new().build()))
        .initial_reconnect_backoff(Duration::from_millis(10))
        .max_reconnect_backoff(Duration::from_millis(500))
        .connect(HARRY_ADDRESS);
    let client = Arc::new(DashboardServiceClient::new(channel));
    let last = Arc::new(Mutex::new(Last::default()));
    info!("pushing to the harry process at {HARRY_ADDRESS}");

    let mut futures_join_set = tokio::task::JoinSet::new();
    futures_join_set.spawn(handle_heartbeat(client.clone(), last.clone()));
    // The Matter devices on their own: their bundle may come late or not at all, and the
    // subscriptions below wait for their publishers one after the other.
    futures_join_set.spawn(matter_link(comms.clone(), client.clone(), last.clone()));
    // The publishers may start later than this bundle: every subscription waits on its own.
    // Every topic is state (the weather now, the feed now, the networks now): a subscription
    // gets the publication's last message at once (read_on_subscribe), so what a bundle
    // published before this one subscribed, its last reading kept from the previous start
    // among it, reaches the screen (the thumbnails of a feed are one message for this).
    let latest = SubscribeOptions::builder().read_on_subscribe(true).build()?;
    let weather = SubscriberDescriptors::<CatalogWeather>::WEATHER;
    if wait_for_publisher_to_be_available(comms.as_ref(), weather).await.is_ok() {
        match clientlib::create_observer(comms.as_ref(), weather, &latest).await {
            Ok(observer) => {
                futures_join_set.spawn(handle_weather_observer(observer, client.clone(), last.clone()));
            }
            Err(e) => error!("Failed to create observer Weather#WEATHER: {e:?}"),
        }
    }
    let networks = SubscriberDescriptors::<CatalogWifiNetworks>::WIFI_NETWORKS;
    if wait_for_publisher_to_be_available(comms.as_ref(), networks).await.is_ok() {
        match clientlib::create_observer(comms.as_ref(), networks, &latest).await {
            Ok(observer) => {
                futures_join_set.spawn(handle_wifi_networks_observer(observer, client.clone(), last.clone()));
            }
            Err(e) => error!("Failed to create observer WifiNetworks#wifi-networks: {e:?}"),
        }
    }
    let state = SubscriberDescriptors::<CatalogWifiState>::WIFI_STATE;
    if wait_for_publisher_to_be_available(comms.as_ref(), state).await.is_ok() {
        match clientlib::create_observer(comms.as_ref(), state, &latest).await {
            Ok(observer) => {
                futures_join_set.spawn(handle_wifi_state_observer(observer, client.clone(), last.clone()));
            }
            Err(e) => error!("Failed to create observer WifiState#wifi-state: {e:?}"),
        }
    }
    let news = SubscriberDescriptors::<CatalogNews>::NEWS;
    if wait_for_publisher_to_be_available(comms.as_ref(), news).await.is_ok() {
        match clientlib::create_observer(comms.as_ref(), news, &latest).await {
            Ok(observer) => {
                futures_join_set.spawn(handle_news_observer(observer, client.clone(), last.clone()));
            }
            Err(e) => error!("Failed to create observer NewsFeed#news: {e:?}"),
        }
    }
    let thumbnails = SubscriberDescriptors::<CatalogThumbnails>::NEWS_THUMBNAILS;
    if wait_for_publisher_to_be_available(comms.as_ref(), thumbnails).await.is_ok() {
        match clientlib::create_observer(comms.as_ref(), thumbnails, &latest).await {
            Ok(observer) => {
                futures_join_set.spawn(handle_thumbnail_observer(observer, client.clone(), last.clone()));
            }
            Err(e) => error!("Failed to create observer NewsThumbnails#news-thumbnails: {e:?}"),
        }
    }
    // The screen's commands go to the Wi-Fi bundle's service once it is there.
    if let Some(wifi) = create_wifi_client(comms.as_ref()).await {
        futures_join_set.spawn(handle_wifi_commands(client, Arc::new(wifi)));
    }
    while let Some(e) = futures_join_set.join_next().await {
        error!("Future joined with result: {e:?}");
    }
    Ok(())
}

/// Every message of the weather topic goes to the harry process; the last one is kept.
async fn handle_weather_observer(
    mut observer: std::pin::Pin<Box<dyn futures::Stream<Item = SdvResult<Vec<CatalogWeather>>> + Send>>,
    client: Arc<DashboardServiceClient>,
    last: Arc<Mutex<Last>>,
) {
    info!("subscribed to Weather#weather");
    while let Some(message) = observer.next().await {
        match message {
            Ok(messages) => {
                for message in messages {
                    // the catalog's type and the gRPC crate's are the same wire format
                    let Ok(bytes) = message.write_to_bytes() else { continue };
                    let Ok(weather) = GrpcWeather::parse_from_bytes(&bytes) else { continue };
                    info!("weather {} °C at {}, {} hours", weather.temperature_c, weather.time, weather.hourly.len());
                    last.lock().expect("last values").weather = Some(weather.clone());
                    push_weather(&client, &weather).await;
                }
            }
            Err(e) => error!("Error receiving message on Weather#WEATHER: {e:?}"),
        }
    }
    warn!("the weather stream ended");
}

/// Every feed of the news topic goes to the harry process; the last one is kept.
async fn handle_news_observer(
    mut observer: std::pin::Pin<Box<dyn futures::Stream<Item = SdvResult<Vec<CatalogNews>>> + Send>>,
    client: Arc<DashboardServiceClient>,
    last: Arc<Mutex<Last>>,
) {
    info!("subscribed to NewsFeed#news");
    while let Some(message) = observer.next().await {
        match message {
            Ok(messages) => {
                for message in messages {
                    let Some(news) = convert::<CatalogNews, GrpcNews>(&message) else { continue };
                    info!("{} news stories", news.stories.len());
                    {
                        let mut last = last.lock().expect("last values");
                        // the thumbnails of stories that left the feed go
                        last.thumbnails.retain(|id, _| news.stories.iter().any(|s| s.id == *id));
                        last.news = Some(news.clone());
                    }
                    push_news(&client, &news).await;
                }
            }
            Err(e) => error!("Error receiving message on NewsFeed#news: {e:?}"),
        }
    }
    warn!("the news stream ended");
}

/// Every thumbnail goes to the harry process; the ones of the current feed are kept.
async fn handle_thumbnail_observer(
    mut observer: std::pin::Pin<Box<dyn futures::Stream<Item = SdvResult<Vec<CatalogThumbnails>>> + Send>>,
    client: Arc<DashboardServiceClient>,
    last: Arc<Mutex<Last>>,
) {
    info!("subscribed to NewsThumbnails#news-thumbnails");
    while let Some(message) = observer.next().await {
        match message {
            Ok(messages) => {
                for message in messages.iter().flat_map(|m| &m.thumbnails) {
                    let Some(thumbnail) = convert::<CatalogThumbnail, GrpcThumbnail>(message) else { continue };
                    debug!("thumbnail of story {}", thumbnail.id);
                    last.lock().expect("last values").thumbnails.insert(thumbnail.id, thumbnail.clone());
                    push_thumbnail(&client, &thumbnail).await;
                }
            }
            Err(e) => error!("Error receiving message on NewsThumbnails#news-thumbnails: {e:?}"),
        }
    }
    warn!("the thumbnail stream ended");
}

/// Every message of the networks topic goes to the harry process; the last one is kept.
async fn handle_wifi_networks_observer(
    mut observer: std::pin::Pin<Box<dyn futures::Stream<Item = SdvResult<Vec<CatalogWifiNetworks>>> + Send>>,
    client: Arc<DashboardServiceClient>,
    last: Arc<Mutex<Last>>,
) {
    info!("subscribed to WifiNetworks#wifi-networks");
    while let Some(message) = observer.next().await {
        match message {
            Ok(messages) => {
                for message in messages {
                    let Some(networks) = convert::<_, GrpcWifiNetworks>(&message) else { continue };
                    info!("{} Wi-Fi networks", networks.networks.len());
                    last.lock().expect("last values").wifi_networks = Some(networks.clone());
                    push_wifi_networks(&client, &networks).await;
                }
            }
            Err(e) => error!("Error receiving message on WifiNetworks#wifi-networks: {e:?}"),
        }
    }
    warn!("the Wi-Fi networks stream ended");
}

/// Every message of the state topic goes to the harry process; the last one is kept.
async fn handle_wifi_state_observer(
    mut observer: std::pin::Pin<Box<dyn futures::Stream<Item = SdvResult<Vec<CatalogWifiState>>> + Send>>,
    client: Arc<DashboardServiceClient>,
    last: Arc<Mutex<Last>>,
) {
    info!("subscribed to WifiState#wifi-state");
    while let Some(message) = observer.next().await {
        match message {
            Ok(messages) => {
                for message in messages {
                    let Some(state) = convert::<_, GrpcWifiState>(&message) else { continue };
                    info!("Wi-Fi {:?} {:?}", state.connection, state.ssid);
                    last.lock().expect("last values").wifi_state = Some(state.clone());
                    push_wifi_state(&client, &state).await;
                }
            }
            Err(e) => error!("Error receiving message on WifiState#wifi-state: {e:?}"),
        }
    }
    warn!("the Wi-Fi state stream ended");
}

/// The Matter devices to the screen and the screen's Matter commands to the Matter bundle, once
/// its publication and its service are there.
async fn matter_link(comms: Arc<dyn Communicate>, app: Arc<DashboardServiceClient>, last: Arc<Mutex<Last>>) {
    let Ok(latest) = SubscribeOptions::builder().read_on_subscribe(true).build() else { return };
    let devices = SubscriberDescriptors::<CatalogMatterDevices>::MATTER_DEVICES;
    if wait_for_publisher_to_be_available(comms.as_ref(), devices).await.is_ok() {
        match clientlib::create_observer(comms.as_ref(), devices, &latest).await {
            Ok(observer) => {
                tokio::spawn(handle_matter_observer(observer, app.clone(), last));
            }
            Err(e) => error!("Failed to create observer MatterDevices#matter-devices: {e:?}"),
        }
    }
    if let Some(matter) = create_matter_client(comms.as_ref()).await {
        handle_matter_commands(app, Arc::new(matter)).await;
    }
}

/// Every message of the Matter topic goes to the harry process; the last one is kept.
async fn handle_matter_observer(
    mut observer: std::pin::Pin<Box<dyn futures::Stream<Item = SdvResult<Vec<CatalogMatterDevices>>> + Send>>,
    client: Arc<DashboardServiceClient>,
    last: Arc<Mutex<Last>>,
) {
    info!("subscribed to MatterDevices#matter-devices");
    while let Some(message) = observer.next().await {
        match message {
            Ok(messages) => {
                for message in messages {
                    let Some(devices) = convert::<_, GrpcMatterDevices>(&message) else { continue };
                    debug!("{} Matter nodes", devices.nodes.len());
                    last.lock().expect("last values").matter_devices = Some(devices.clone());
                    push_matter_devices(&client, &devices).await;
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

/// The screen's Matter commands, as the Wi-Fi ones (handle_wifi_commands).
async fn handle_matter_commands(app: Arc<DashboardServiceClient>, matter: Arc<MatterClient>) {
    loop {
        match app.matter_commands(&Subscribe::new()) {
            Ok(mut stream) => {
                let mut taking = false;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(command) => {
                            if !taking {
                                info!("taking Matter commands from the harry process");
                                taking = true;
                            }
                            let Some(command) = convert::<GrpcMatterCommand, CatalogMatterCommand>(&command) else { continue };
                            tokio::spawn(call_matter(app.clone(), matter.clone(), command));
                        }
                        Err(err) => {
                            debug!("the Matter command stream ended: {err}");
                            break;
                        }
                    }
                }
            }
            Err(err) => debug!("no Matter command stream: {err}"),
        }
        sleep(HEARTBEAT).await;
    }
}

/// One Matter command, then its answer to the screen (a call that did not get through too).
async fn call_matter(app: Arc<DashboardServiceClient>, matter: Arc<MatterClient>, command: CatalogMatterCommand) {
    let (name, node_id, endpoint, answer) = match command.command {
        Some(MatterCmd::Pair(r)) => ("pair", 0, 0, matter.Pair(&r).await),
        Some(MatterCmd::SetOnOff(r)) => ("on-off", r.node_id, r.endpoint, matter.SetOnOff(&r).await),
        Some(MatterCmd::MoveToLevel(r)) => ("level", r.node_id, r.endpoint, matter.MoveToLevel(&r).await),
        Some(MatterCmd::SetSetpoint(r)) => ("setpoint", r.node_id, r.endpoint, matter.SetSetpoint(&r).await),
        Some(MatterCmd::SetSystemMode(r)) => ("system-mode", r.node_id, r.endpoint, matter.SetSystemMode(&r).await),
        Some(MatterCmd::SetFanMode(r)) => ("fan-mode", r.node_id, r.endpoint, matter.SetFanMode(&r).await),
        Some(_) | None => return,
    };
    let result = match answer {
        Ok(result) => {
            info!("Matter {name} node {node_id:#x} ep{endpoint}: ok {} {:?}", result.ok, result.error);
            result
        }
        Err(e) => {
            warn!("Matter {name} node {node_id:#x} ep{endpoint}: the call failed: {e:?}");
            let mut result = CatalogMatterResult::new();
            result.error = "the Matter service did not answer".to_string();
            result
        }
    };
    let mut message = MatterCommandResult::new();
    message.command = name.to_string();
    message.node_id = node_id;
    message.endpoint = endpoint;
    message.result = convert::<CatalogMatterResult, GrpcMatterResult>(&result).into();
    match app.publish_matter_result_async(&message) {
        Ok(reply) => {
            if let Err(e) = reply.await {
                debug!("the screen did not take the Matter result: {e}");
            }
        }
        Err(e) => debug!("the screen did not take the Matter result: {e}"),
    }
}

async fn push_matter_devices(client: &DashboardServiceClient, devices: &GrpcMatterDevices) {
    match client.publish_matter_devices_async(devices) {
        Ok(reply) => {
            if let Err(err) = reply.await {
                debug!("publish Matter devices: {err}");
            }
        }
        Err(err) => debug!("publish Matter devices: {err}"),
    }
}

/// The client of the Wi-Fi bundle's WifiService, once the server is registered (the bundles
/// start in any order).
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

/// The screen's Wi-Fi commands: a server stream from the harry process, opened again
/// whenever it ends (the process restarted). Every command is a call of the Wi-Fi bundle's
/// service in a task of its own (a connect waits for its outcome; a scan meanwhile does not
/// wait for it), and its answer goes back to the screen.
async fn handle_wifi_commands(app: Arc<DashboardServiceClient>, wifi: Arc<WifiClient>) {
    loop {
        match app.wifi_commands(&Subscribe::new()) {
            Ok(mut stream) => {
                // The stream opens lazily: it is real once the first command arrives, and it
                // ends at once while the harry process is not there (every second then).
                let mut taking = false;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(command) => {
                            if !taking {
                                info!("taking Wi-Fi commands from the harry process");
                                taking = true;
                            }
                            let Some(command) = convert::<GrpcWifiCommand, CatalogWifiCommand>(&command) else { continue };
                            tokio::spawn(call_wifi(app.clone(), wifi.clone(), command));
                        }
                        Err(err) => {
                            debug!("the command stream ended: {err}");
                            break;
                        }
                    }
                }
            }
            Err(err) => debug!("no command stream: {err}"),
        }
        sleep(HEARTBEAT).await;
    }
}

/// One command: the call, then its answer to the screen (a call that did not get through
/// is an answer too: the screen must not wait for a connect that never started).
async fn call_wifi(app: Arc<DashboardServiceClient>, wifi: Arc<WifiClient>, command: CatalogWifiCommand) {
    let (name, ssid, answer) = match command.command {
        Some(Command::Scan(scan)) => ("scan", String::new(), wifi.Scan(&scan).await.map(|_| ok())),
        Some(Command::Connect(connect)) => ("connect", connect.ssid.clone(), wifi.Connect(&connect).await),
        Some(Command::Forget(forget)) => ("forget", forget.ssid.clone(), wifi.Forget(&forget).await),
        Some(Command::Disconnect(disconnect)) => ("disconnect", String::new(), wifi.Disconnect(&disconnect).await),
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
            let mut result = CatalogWifiResult::new();
            result.error = "the Wi-Fi service did not answer".to_string();
            result
        }
    };
    let mut message = WifiCommandResult::new();
    message.command = name.to_string();
    message.ssid = ssid;
    message.result = convert::<CatalogWifiResult, GrpcWifiResult>(&result).into();
    match app.publish_wifi_result_async(&message) {
        Ok(reply) => {
            if let Err(e) = reply.await {
                debug!("the screen did not take the Wi-Fi result: {e}");
            }
        }
        Err(e) => debug!("the screen did not take the Wi-Fi result: {e}"),
    }
}

fn ok() -> CatalogWifiResult {
    let mut result = CatalogWifiResult::new();
    result.ok = true;
    result
}

/// A heartbeat a second. When the link comes back after a failure, the last values go out
/// again: the harry process may have restarted and start from nothing.
async fn handle_heartbeat(client: Arc<DashboardServiceClient>, last: Arc<Mutex<Last>>) {
    let started = Instant::now();
    let mut linked = false;
    loop {
        sleep(HEARTBEAT).await;
        let mut request = HeartbeatRequest::new();
        request.uptime_s = started.elapsed().as_secs();
        let sent = match client.heartbeat_async(&request) {
            Ok(reply) => reply.await.is_ok(),
            Err(_) => false,
        };
        if sent && !linked {
            info!("the harry process is listening");
            let (weather, news, thumbnails, networks, state, matter) = {
                let last = last.lock().expect("last values");
                (last.weather.clone(), last.news.clone(), last.thumbnails.values().cloned().collect::<Vec<_>>(), last.wifi_networks.clone(), last.wifi_state.clone(), last.matter_devices.clone())
            };
            if let Some(weather) = weather {
                push_weather(&client, &weather).await;
            }
            if let Some(news) = news {
                push_news(&client, &news).await;
            }
            for thumbnail in &thumbnails {
                push_thumbnail(&client, thumbnail).await;
            }
            if let Some(networks) = networks {
                push_wifi_networks(&client, &networks).await;
            }
            if let Some(state) = state {
                push_wifi_state(&client, &state).await;
            }
            if let Some(matter) = matter {
                push_matter_devices(&client, &matter).await;
            }
        } else if !sent && linked {
            warn!("the harry process is gone, keeping the last values");
        }
        linked = sent;
    }
}

async fn push_news(client: &DashboardServiceClient, news: &GrpcNews) {
    if let Err(err) = match client.publish_news_async(news) {
        Ok(reply) => reply.await.map(|_| ()),
        Err(err) => Err(err),
    } {
        warn!("publish news: {err}; the harry process gets it when it is back");
    }
}

async fn push_thumbnail(client: &DashboardServiceClient, thumbnail: &GrpcThumbnail) {
    if let Err(err) = match client.publish_news_thumbnail_async(thumbnail) {
        Ok(reply) => reply.await.map(|_| ()),
        Err(err) => Err(err),
    } {
        debug!("publish thumbnail: {err}");
    }
}

async fn push_weather(client: &DashboardServiceClient, weather: &GrpcWeather) {
    let sent = match client.publish_weather_async(weather) {
        Ok(reply) => reply.await.map_err(|err| warn!("publish weather: {err}")).is_ok(),
        Err(err) => {
            warn!("publish weather: {err}");
            false
        }
    };
    if !sent {
        warn!("the harry process did not take the weather; it gets it when it is back");
    }
}

async fn push_wifi_networks(client: &DashboardServiceClient, networks: &GrpcWifiNetworks) {
    match client.publish_wifi_networks_async(networks) {
        Ok(reply) => {
            if let Err(err) = reply.await {
                warn!("publish Wi-Fi networks: {err}");
            }
        }
        Err(err) => warn!("publish Wi-Fi networks: {err}"),
    }
}

async fn push_wifi_state(client: &DashboardServiceClient, state: &GrpcWifiState) {
    match client.publish_wifi_state_async(state) {
        Ok(reply) => {
            if let Err(err) = reply.await {
                warn!("publish Wi-Fi state: {err}");
            }
        }
        Err(err) => warn!("publish Wi-Fi state: {err}"),
    }
}
