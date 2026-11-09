//! MatterServiceBundle: owns the house's Matter fabric. It commissions the devices whose pairing
//! codes it is given, reads what their clusters report every two seconds and publishes all of
//! them as one message on the topic `matter-devices` (catalog/matter.proto), and offers the
//! MatterService RPC (catalog/matter_service.proto): pairing and the devices' commands, each
//! answered when the device acknowledged it.
//!
//! The shape is the service bundle vsidlc generates (`vsidlc --services`): a `ServiceBundle`
//! with `SdvComms`, a current-thread runtime, `sdv_main` with the publisher and the RPC server.
//! The Matter side is `everyware_matter_client` (client/): a controller on a thread of its own.

use everyware_catalog::matter::{MatterDevices, MatterEndpoint, MatterFan, MatterHumidity, MatterLevel, MatterNode, MatterOnOff, MatterThermostat};
use everyware_catalog::matter_service::{MatterMoveToLevel, MatterPair, MatterResult, MatterSetFanMode, MatterSetOnOff, MatterSetSetpoint, MatterSetSystemMode};
use everyware_matter_client::{Command, Config, Controller, Snapshot};
use log::{error, info, warn};
use sdv::mw::{clientlib, Communicate, Publisher, SdvComms};
use sdv::status::{SdvResult, SdvStatus, SdvStatusCode};
use sdv_mw_rs_com_everyware_matter_service_bundle::{MatterServiceBundle as Bundle, PublisherDescriptors};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// The interface of the house's network.
const INTERFACE: &str = "wlan0";
/// How often the devices are read (subscriptions later).
const POLL: Duration = Duration::from_secs(2);
/// The pairing codes of the devices to commission, one per line (the bundle's configuration in
/// its APEX).
const PAIRING_CODES: &str = "/apex/com.everyware.matter/etc/matter/pairing.txt";
/// A command waits this long for the device; pairing longer (two phases of up to a minute).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const PAIR_TIMEOUT: Duration = Duration::from_secs(150);

sdv::lifecycle::register_service_bundle!(MatterServiceBundle);

struct MatterServiceBundle {
    cancellation_token: Option<CancellationToken>,
    context: ContextRef,
    /// The bundle's directory (/data/service_bundle_data/<package>/<bundle>).
    dir: PathBuf,
}

impl ServiceBundle for MatterServiceBundle {
    fn new(context: ContextRef) -> Self {
        if let Err(err) = sdv::tracing::try_init_tracing() {
            warn!("Failed to init service bundles tracing: {err}");
        }
        let fqin = context.get_self_fqin();
        // as SDV's samples build it (no API for the path yet, b/381209295)
        let dir = PathBuf::from(format!("/data/service_bundle_data/{}/{}", fqin.get_sdv_package_name(), fqin.get_service_bundle_name()));
        Self { cancellation_token: None, context, dir }
    }

    fn on_start(&mut self) {
        let context = self.context;
        let dir = self.dir.clone();
        let cancellation_token = CancellationToken::new();
        self.cancellation_token = Some(cancellation_token.clone());
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_time().enable_io().build() else {
            panic!("Failed to start the runtime!");
        };
        let _ = std::thread::spawn(move || {
            runtime.block_on(async move {
                let comms = Arc::new(SdvComms { context });
                let res = tokio::select! {
                    res = sdv_main(comms, dir) => res,
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

async fn sdv_main(comms: Arc<dyn Communicate>, dir: PathBuf) -> SdvResult<()> {
    info!("Starting service bundle 'MatterServiceBundle'");
    let codes: Vec<String> = std::fs::read_to_string(PAIRING_CODES)
        .map(|text| text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string).collect())
        .unwrap_or_else(|err| {
            warn!("no pairing codes ({PAIRING_CODES}: {err})");
            Vec::new()
        });
    let (snapshots_tx, mut snapshots) = mpsc::unbounded_channel::<Snapshot>();
    let config = Config { dir, interface: INTERFACE.to_string(), codes, poll: POLL };
    info!("controller in {}, on {INTERFACE}, {} pairing code(s)", config.dir.display(), config.codes.len());
    let controller = Arc::new(
        Controller::start(config, Box::new(move |s: &Snapshot| {
            let _ = snapshots_tx.send(s.clone());
        }))
        .map_err(|e| SdvStatus::with_message(SdvStatusCode::Internal, format!("the Matter controller: {e}")))?,
    );
    let _service_bundle = match Bundle::builder().set_comms(comms.clone()).set_server_matter(Arc::new(MatterServiceRpc { controller })).build().await {
        Ok(service) => service,
        Err(e) => {
            error!("Failed to build service bundle: {e}");
            return Err(SdvStatus::with_message(SdvStatusCode::Internal, e.to_string()));
        }
    };
    let publisher = clientlib::create_publisher(comms.as_ref(), PublisherDescriptors::<MatterDevices>::MATTER_DEVICES).await.map_err(|e| {
        error!("Failed to create publisher MatterDevices#matter-devices: {e:?}");
        e
    })?;
    info!("serving MatterService on the channel matter");
    publish_loop(publisher, &mut snapshots).await;
    Ok(())
}

/// Every poll that changed something goes out.
async fn publish_loop(publisher: Publisher<MatterDevices>, snapshots: &mut mpsc::UnboundedReceiver<Snapshot>) {
    let mut last: Option<MatterDevices> = None;
    while let Some(snapshot) = snapshots.recv().await {
        let message = to_message(&snapshot);
        if last.as_ref() == Some(&message) {
            continue;
        }
        match publisher.publish(&message) {
            Ok(()) => last = Some(message),
            Err(e) => error!("Failed to publish on MatterDevices#matter-devices: {e:?}"),
        }
    }
    warn!("the Matter controller stopped");
}

/// The snapshot as the catalog's message (strings within the catalog's sizes).
fn to_message(s: &Snapshot) -> MatterDevices {
    let cut = |text: &str, max: usize| -> String {
        let mut end = text.len().min(max);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].to_string()
    };
    let mut m = MatterDevices::new();
    m.nodes = s
        .nodes
        .iter()
        .take(8)
        .map(|n| {
            let mut node = MatterNode::new();
            node.node_id = n.node_id;
            node.online = n.online;
            node.last_seen = n.last_seen;
            node.vendor = cut(&n.vendor, 32);
            node.product = cut(&n.product, 32);
            node.serial_number = cut(&n.serial_number, 32);
            node.software_version = cut(&n.software_version, 64);
            node.vendor_id = u32::from(n.vendor_id);
            node.product_id = u32::from(n.product_id);
            node.uptime_s = n.uptime_s;
            node.boot_reason = cut(&n.boot_reason, 32);
            node.endpoints = n
                .endpoints
                .iter()
                .take(8)
                .map(|e| {
                    let mut ep = MatterEndpoint::new();
                    ep.id = u32::from(e.id);
                    ep.device_type = e.device_type;
                    if let Some(t) = &e.thermostat {
                        let mut x = MatterThermostat::new();
                        x.local_temperature_c = t.local_temperature_c.unwrap_or_default();
                        x.local_temperature_known = t.local_temperature_c.is_some();
                        x.heating_setpoint_c = t.heating_setpoint_c;
                        x.cooling_setpoint_c = t.cooling_setpoint_c;
                        x.system_mode = cut(&t.system_mode, 16);
                        x.running = cut(&t.running, 32);
                        x.min_heat_setpoint_c = t.min_heat_setpoint_c;
                        x.max_heat_setpoint_c = t.max_heat_setpoint_c;
                        x.min_cool_setpoint_c = t.min_cool_setpoint_c;
                        x.max_cool_setpoint_c = t.max_cool_setpoint_c;
                        ep.thermostat = Some(x).into();
                    }
                    if let Some(h) = &e.humidity {
                        let mut x = MatterHumidity::new();
                        x.percent = h.percent.unwrap_or_default();
                        x.known = h.percent.is_some();
                        ep.humidity = Some(x).into();
                    }
                    if let Some(o) = &e.on_off {
                        let mut x = MatterOnOff::new();
                        x.on = o.on;
                        x.start_up = cut(&o.start_up, 16);
                        ep.on_off = Some(x).into();
                    }
                    if let Some(l) = &e.level {
                        let mut x = MatterLevel::new();
                        x.level = l.level.map(u32::from).unwrap_or(0);
                        x.min_level = u32::from(l.min_level);
                        x.max_level = u32::from(l.max_level);
                        x.transition_ds = u32::from(l.transition_ds);
                        x.start_up_level = l.start_up_level.map(u32::from).unwrap_or(0);
                        ep.level = Some(x).into();
                    }
                    if let Some(f) = &e.fan {
                        let mut x = MatterFan::new();
                        x.mode = cut(&f.mode, 16);
                        x.mode_sequence = cut(&f.mode_sequence, 24);
                        x.percent_setting = f.percent_setting.map(u32::from).unwrap_or(0);
                        x.percent_current = u32::from(f.percent_current);
                        x.airflow_direction = cut(&f.airflow_direction, 16);
                        x.rock_support = cut(&f.rock_support, 48);
                        x.rock_setting = cut(&f.rock_setting, 48);
                        x.wind_support = cut(&f.wind_support, 48);
                        x.wind_setting = cut(&f.wind_setting, 48);
                        ep.fan = Some(x).into();
                    }
                    ep
                })
                .collect();
            node
        })
        .collect();
    m
}

/// The MatterService server: every call becomes a command to the controller and waits for its
/// answer.
struct MatterServiceRpc {
    controller: Arc<Controller>,
}

impl MatterServiceRpc {
    async fn ask(&self, command: Command, wait: Duration) -> SdvResult<MatterResult> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.controller.send(
            command,
            Box::new(move |outcome| {
                let _ = reply_tx.send(outcome);
            }),
        );
        let outcome = match timeout(wait, reply_rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err("the Matter controller is gone".to_string()),
            Err(_) => Err("no answer in time".to_string()),
        };
        let mut result = MatterResult::new();
        match outcome {
            Ok(node_id) => {
                result.ok = true;
                result.node_id = node_id;
            }
            Err(why) => result.error = why.chars().take(96).collect(),
        }
        Ok(result)
    }
}

fn endpoint(e: u32) -> SdvResult<u16> {
    u16::try_from(e).map_err(|_| SdvStatus::with_message(SdvStatusCode::InvalidArgument, "endpoint out of range"))
}

#[async_trait::async_trait]
#[allow(non_snake_case)]
impl com_everyware_matter_service_rpc::Interface for MatterServiceRpc {
    async fn Pair(&self, _caller_id: sdv::comms::id::ServiceFqin, request: &MatterPair) -> SdvResult<MatterResult> {
        self.ask(Command::Pair { code: request.code.clone() }, PAIR_TIMEOUT).await
    }

    async fn SetOnOff(&self, _caller_id: sdv::comms::id::ServiceFqin, r: &MatterSetOnOff) -> SdvResult<MatterResult> {
        self.ask(Command::SetOnOff { node_id: r.node_id, endpoint: endpoint(r.endpoint)?, on: r.on }, COMMAND_TIMEOUT).await
    }

    async fn MoveToLevel(&self, _caller_id: sdv::comms::id::ServiceFqin, r: &MatterMoveToLevel) -> SdvResult<MatterResult> {
        let level = u8::try_from(r.level.clamp(1, 254)).unwrap_or(254);
        let transition_ds = (r.transition_ds > 0).then(|| u16::try_from(r.transition_ds).unwrap_or(u16::MAX));
        self.ask(Command::MoveToLevel { node_id: r.node_id, endpoint: endpoint(r.endpoint)?, level, transition_ds }, COMMAND_TIMEOUT).await
    }

    async fn SetSetpoint(&self, _caller_id: sdv::comms::id::ServiceFqin, r: &MatterSetSetpoint) -> SdvResult<MatterResult> {
        self.ask(Command::SetSetpoint { node_id: r.node_id, endpoint: endpoint(r.endpoint)?, heating: r.heating, celsius: r.celsius }, COMMAND_TIMEOUT).await
    }

    async fn SetSystemMode(&self, _caller_id: sdv::comms::id::ServiceFqin, r: &MatterSetSystemMode) -> SdvResult<MatterResult> {
        self.ask(Command::SetSystemMode { node_id: r.node_id, endpoint: endpoint(r.endpoint)?, mode: r.mode.clone() }, COMMAND_TIMEOUT).await
    }

    async fn SetFanMode(&self, _caller_id: sdv::comms::id::ServiceFqin, r: &MatterSetFanMode) -> SdvResult<MatterResult> {
        self.ask(Command::SetFanMode { node_id: r.node_id, endpoint: endpoint(r.endpoint)?, mode: r.mode.clone() }, COMMAND_TIMEOUT).await
    }
}
