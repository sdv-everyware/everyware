//! The house's Matter controller (rs-matter): it owns the house's fabric, commissions devices by
//! their pairing codes (found on the network over mDNS), reads what their clusters report every
//! poll and runs the commands it is given. It runs on a thread of its own (rs-matter's
//! futures run on async-io, not on the bundle's tokio); the bundle talks to it through
//! `Controller::send` and gets every poll's `Snapshot` through a callback.
//!
//! Its files are in the directory it is given (the bundle's, /data/service_bundle_data/...):
//! the fabric (fabric.rs) and the nodes commissioned onto it.

pub mod fabric;
pub mod model;
pub mod net;
mod node;

pub use model::{Command, EndpointState, Fan, Humidity, Level, NodeState, OnOff, Reply, Snapshot, Thermostat};

use fabric::{Ca, PairedNode};
use log::{error, info, warn};
use rs_matter::cert::gen::VALID_FOREVER;
use rs_matter::cert::{MAX_CERT_TLV_AND_ASN1_LEN, MAX_CERT_TLV_LEN};
use rs_matter::crypto::{default_crypto, Crypto};
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::onboard::noc::NocGenerator;
use rs_matter::onboard::{CommissionOptions, Commissioner};
use rs_matter::pairing::qr::QrPayload;
use rs_matter::transport::network::mdns::builtin::{BuiltinMdns, Host};
use rs_matter::transport::network::mdns::{CommissionableFilter, MdnsBrowseMatch, MdnsBrowseQuery};
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::futures::{select, Either};
use rs_matter::utils::init::InitMaybeUninit;
use rs_matter::utils::timers::{Duration as MatterDuration, Timer};
use rs_matter::Matter;
use std::num::NonZeroU8;
use std::future::Future;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How the controller is set up.
#[derive(Clone, Debug)]
pub struct Config {
    /// Where the fabric and the node list are kept.
    pub dir: PathBuf,
    /// The interface of the house's network ("wlan0").
    pub interface: String,
    /// The pairing codes of the devices to commission (each once).
    pub codes: Vec<String>,
    /// How often the devices are read.
    pub poll: Duration,
}

/// The controller, from the bundle's side.
pub struct Controller {
    commands: mpsc::Sender<(Command, Reply)>,
}

/// A poll's result for the bundle.
pub type OnSnapshot = Box<dyn Fn(&Snapshot) + Send>;

impl Controller {
    /// Starts the controller's thread.
    pub fn start(config: Config, on_snapshot: OnSnapshot) -> std::io::Result<Self> {
        let (commands, requests) = mpsc::channel();
        // rs-matter's futures are large (its buffers live in them)
        thread::Builder::new().name("matter".into()).stack_size(16 << 20).spawn(move || run(config, requests, on_snapshot))?;
        Ok(Controller { commands })
    }

    /// Runs `command`; `reply` gets its outcome.
    pub fn send(&self, command: Command, reply: Reply) {
        if let Err(mpsc::SendError((_, reply))) = self.commands.send((command, reply)) {
            reply(Err("the Matter controller is gone".into()));
        }
    }
}

/// How many polls a node may miss before it counts as offline.
const MISSES_OFFLINE: u32 = 3;
/// How long one commissioning phase may take.
const COMMISSION_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the mDNS browse for a pairing code collects answers.
const BROWSE_WINDOW_MS: u32 = 2000;
/// A code that could not be paired is tried again after this.
const PAIR_RETRY: Duration = Duration::from_secs(30);
/// The network is looked for again after this (at boot the Wi-Fi comes up after the bundle).
const NETWORK_RETRY: Duration = Duration::from_secs(2);

/// The controller's thread: one Matter node for the process, sessions on the network until
/// the commands' sender is gone.
fn run(config: Config, requests: mpsc::Receiver<(Command, Reply)>, on_snapshot: OnSnapshot) {
    // The node lives as long as the process: rs-matter's transport borrows it for good.
    let matter: &'static Matter<'static> =
        Box::leak(Box::<Matter<'static>>::new_uninit()).init_with(Matter::init(&TEST_DEV_DET, TEST_DEV_COMM, &TEST_DEV_ATT, 0));
    let crypto = default_crypto(rand::rng(), DAC_PRIVKEY);

    let (ca, fab_idx) = match install_fabric(matter, &crypto, &config) {
        Ok(installed) => installed,
        Err(err) => {
            error!("matter: the fabric cannot be set up: {err:?}");
            return;
        }
    };
    let mut nodes = fabric::load_nodes(&config.dir).unwrap_or_else(|err| {
        warn!("matter: the node list: {err}");
        Vec::new()
    });
    let mut states: Vec<NodeState> = Vec::new();
    loop {
        let interface = match net::interface(&config.interface) {
            Ok(interface) => interface,
            Err(why) => {
                info!("matter: waiting for the network: {why}");
                thread::sleep(NETWORK_RETRY);
                continue;
            }
        };
        info!("matter: on {} (index {}), {} / {:?}", interface.name, interface.index, interface.ipv4, interface.ipv6);
        let session = session(matter, &crypto, &ca, fab_idx, &config, &interface, &mut nodes, &mut states, &requests, &on_snapshot);
        match async_io::block_on(session) {
            Ok(()) => return,
            Err(err) => {
                warn!("matter: the session ended: {err:?}; again");
                thread::sleep(NETWORK_RETRY);
            }
        }
    }
}

/// The house's fabric in rs-matter: the one kept, else a new one (kept then).
fn install_fabric<C: Crypto>(matter: &Matter<'_>, crypto: &C, config: &Config) -> Result<(Ca, NonZeroU8), Error> {
    let ca = match Ca::load(&config.dir).map_err(fabric::io_error)? {
        Some(ca) => {
            info!("matter: fabric {:016X} from {}", ca.fabric_id, config.dir.display());
            ca
        }
        None => {
            let ca = Ca::create(crypto)?;
            ca.save(&config.dir).map_err(fabric::io_error)?;
            info!("matter: new fabric {:016X}, kept in {}", ca.fabric_id, config.dir.display());
            ca
        }
    };
    let fab_idx = matter.with_state(|state| {
        state
            .fabrics
            .add(crypto, ca.key.reference(), &ca.rcac, &ca.noc, &ca.icac, Some(ca.ipk.reference()), fabric::ADMIN_VENDOR_ID, fabric::CONTROLLER_NODE_ID)
            .map(|f| f.fab_idx())
    })?;
    Ok((ca, fab_idx))
}

/// One session on `interface`: Matter's transport, the mDNS responder and the work, until one
/// of them fails (the network went away) or the bundle is gone (Ok).
#[allow(clippy::too_many_arguments)]
async fn session<C: Crypto>(
    matter: &'static Matter<'static>,
    crypto: &C,
    ca: &Ca,
    fab_idx: NonZeroU8,
    config: &Config,
    interface: &net::Interface,
    nodes: &mut Vec<PairedNode>,
    states: &mut Vec<NodeState>,
    requests: &mpsc::Receiver<(Command, Reply)>,
    on_snapshot: &OnSnapshot,
) -> Result<(), Error> {
    let socket = net::matter_socket().map_err(fabric::io_error)?;
    let mdns_socket = net::mdns_socket(interface).map_err(fabric::io_error)?;
    let hostname = format!("{:016X}", ca.fabric_id ^ fabric::CONTROLLER_NODE_ID);
    let host = Host { hostname: &hostname, ip: interface.ipv4, ipv6: &interface.ipv6 };
    let mut mdns = BuiltinMdns::new();

    let transport = pin!(matter.run(crypto, &socket, &socket, NoNetwork));
    let responder = pin!(mdns.run(&mdns_socket, &mdns_socket, &host, Some(interface.ipv4), Some(interface.index), matter, crypto));
    let work = pin!(work(matter, crypto, ca, fab_idx, config, nodes, states, requests, on_snapshot));

    match select(select(transport, responder), work).await {
        Either::First(Either::First(r)) => r.and(Err(ErrorCode::NoNetworkInterface.into())),
        Either::First(Either::Second(r)) => r.and(Err(ErrorCode::MdnsError.into())),
        Either::Second(r) => r,
    }
}

/// Pairs the configured codes not paired yet, polls the nodes, runs the commands.
#[allow(clippy::too_many_arguments)]
async fn work<C: Crypto>(
    matter: &'static Matter<'static>,
    crypto: &C,
    ca: &Ca,
    fab_idx: NonZeroU8,
    config: &Config,
    nodes: &mut Vec<PairedNode>,
    states: &mut Vec<NodeState>,
    requests: &mpsc::Receiver<(Command, Reply)>,
    on_snapshot: &OnSnapshot,
) -> Result<(), Error> {
    let mut next_poll = Instant::now();
    let mut next_pair = Instant::now();
    let mut misses: Vec<u32> = Vec::new();
    let mut structure: Vec<Option<Vec<node::EndpointInfo>>> = Vec::new();
    loop {
        // the commands first: the screen waits for them
        loop {
            match requests.try_recv() {
                Ok((command, reply)) => {
                    let outcome = run_command(matter, crypto, ca, fab_idx, config, nodes, &command).await;
                    if let Err(why) = &outcome {
                        warn!("matter: {command:?}: {why}");
                    } else {
                        info!("matter: {command:?}: ok");
                        // the screen sees the effect at once
                        next_poll = Instant::now();
                    }
                    reply(outcome);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if Instant::now() >= next_pair {
            next_pair = Instant::now() + PAIR_RETRY;
            for code in &config.codes {
                if nodes.iter().any(|n| n.code == *code) {
                    continue;
                }
                match pair(matter, crypto, ca, fab_idx, code, fabric::next_node_id(nodes)).await {
                    Ok(node_id) => {
                        nodes.push(PairedNode { node_id, code: code.clone() });
                        if let Err(err) = fabric::save_nodes(&config.dir, nodes) {
                            warn!("matter: the node list not kept: {err}");
                        }
                    }
                    Err(err) => warn!("matter: pairing {code}: {err:?}; again in {} s", PAIR_RETRY.as_secs()),
                }
            }
        }
        if Instant::now() >= next_poll {
            next_poll = Instant::now() + config.poll;
            poll(matter, crypto, fab_idx, nodes, states, &mut misses, &mut structure).await;
            on_snapshot(&Snapshot { nodes: states.clone() });
        }
        Timer::after(MatterDuration::from_millis(100)).await;
    }
}

/// Reads every node once; a node that does not answer keeps its last values, and after
/// MISSES_OFFLINE polls counts as offline.
async fn poll<C: Crypto>(
    matter: &'static Matter<'static>,
    crypto: &C,
    fab_idx: NonZeroU8,
    nodes: &[PairedNode],
    states: &mut Vec<NodeState>,
    misses: &mut Vec<u32>,
    structure: &mut Vec<Option<Vec<node::EndpointInfo>>>,
) {
    states.resize_with(nodes.len(), NodeState::default);
    misses.resize(nodes.len(), 0);
    structure.resize(nodes.len(), None);
    for (i, paired) in nodes.iter().enumerate() {
        let reader = node::Reader { matter, crypto, fab_idx, node_id: paired.node_id };
        // the node's endpoints and clusters, and what does not change, once per answer streak
        if structure[i].is_none() {
            match reader.structure().await {
                Ok(endpoints) => {
                    info!("matter: node {:#x}: {}", paired.node_id, node::describe(&endpoints));
                    structure[i] = Some(endpoints);
                }
                Err(err) => {
                    missed(&mut states[i], &mut misses[i], paired.node_id, err);
                    continue;
                }
            }
        }
        let endpoints = structure[i].as_ref().expect("read above");
        match reader.state(endpoints, &states[i]).await {
            Ok(mut state) => {
                state.online = true;
                state.last_seen = unix_now();
                if states[i].endpoints != state.endpoints || !states[i].online {
                    info!("matter: node {:#x}: {}", paired.node_id, node::summary(&state));
                }
                states[i] = state;
                misses[i] = 0;
            }
            Err(err) => {
                missed(&mut states[i], &mut misses[i], paired.node_id, err);
                // read the structure again when it answers: it may have been reset
                structure[i] = None;
            }
        }
    }
}

fn missed(state: &mut NodeState, misses: &mut u32, node_id: u64, err: Error) {
    *misses += 1;
    state.node_id = node_id;
    if *misses == MISSES_OFFLINE {
        warn!("matter: node {node_id:#x} offline: {err:?}");
    }
    state.online = *misses < MISSES_OFFLINE && state.online;
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Commissions the device with `code`, found by mDNS, onto the fabric as `node_id`.
async fn pair<C: Crypto>(matter: &'static Matter<'static>, crypto: &C, ca: &Ca, fab_idx: NonZeroU8, code: &str, node_id: u64) -> Result<u64, Error> {
    let payload = QrPayload::parse_pairing_code(code)?;
    let filter = CommissionableFilter::for_pairing_code(&payload);
    let mut found = Vec::new();
    matter
        .transport()
        .browse(&MdnsBrowseQuery::Commissionable(filter), BROWSE_WINDOW_MS, |m| {
            if let MdnsBrowseMatch::Commissionable { id } = m {
                found.push(id);
            }
        })
        .await?;
    if found.is_empty() {
        info!("matter: no device in commissioning mode for {code} yet");
        return Err(ErrorCode::NotFound.into());
    }
    for id in found {
        let addrs = matter.transport().resolve_commissionable_addrs(id).await?;
        for addr in addrs.iter() {
            info!("matter: pairing {code}: commissionable {id:016X} at {addr:?}, node id {node_id:#x}");
            match commission(matter, crypto, ca, fab_idx, *addr, payload.passcode(), node_id).await {
                Ok(()) => {
                    info!("matter: paired {code} as node {node_id:#x}");
                    return Ok(node_id);
                }
                Err(err) => warn!("matter: pairing {code} at {addr:?}: {err:?}"),
            }
        }
    }
    Err(ErrorCode::Failure.into())
}

/// Both phases of commissioning: PASE and the fabric's certificates, then CASE and
/// CommissioningComplete.
async fn commission<C: Crypto>(
    matter: &'static Matter<'static>,
    crypto: &C,
    ca: &Ca,
    fab_idx: NonZeroU8,
    addr: rs_matter::transport::network::Address,
    passcode: u32,
    node_id: u64,
) -> Result<(), Error> {
    let mut noc_buf = [0u8; MAX_CERT_TLV_AND_ASN1_LEN];
    let mut noc_gen = NocGenerator::create(ca.icac_key.reference(), &ca.rcac, &ca.icac, &mut noc_buf)?;
    let mut buf = [0u8; MAX_CERT_TLV_LEN];
    let mut commissioner = Commissioner::new(matter, crypto, fab_idx, &mut noc_gen, &mut buf);
    // The devices are test devices (test attestation): verification against the DCL is
    // rs-matter's future work.
    let opts = CommissionOptions { allow_test_attestation: true, ..CommissionOptions::default() };
    let phase1 = within(COMMISSION_TIMEOUT, commissioner.commission(addr, passcode, &opts, node_id, VALID_FOREVER)).await??;
    info!("matter: node {node_id:#x}: on the fabric (phase 1), the device's fabric index {}", phase1.fabric_index);
    within(COMMISSION_TIMEOUT, commissioner.complete_via_case(&phase1)).await??;
    info!("matter: node {node_id:#x}: CASE, commissioning complete (phase 2)");
    Ok(())
}

/// `fut`, or a timeout after `limit`.
async fn within<T>(limit: Duration, fut: impl Future<Output = T>) -> Result<T, Error> {
    let fut = pin!(fut);
    let timer = pin!(Timer::after(MatterDuration::from_millis(limit.as_millis() as u64)));
    match select(fut, timer).await {
        Either::First(r) => Ok(r),
        Either::Second(_) => Err(ErrorCode::RxTimeout.into()),
    }
}

async fn run_command<C: Crypto>(
    matter: &'static Matter<'static>,
    crypto: &C,
    ca: &Ca,
    fab_idx: NonZeroU8,
    config: &Config,
    nodes: &mut Vec<PairedNode>,
    command: &Command,
) -> Result<u64, String> {
    let known = |id: u64| nodes.iter().any(|n| n.node_id == id);
    let result = match command {
        Command::Pair { code } => {
            if let Some(n) = nodes.iter().find(|n| n.code == *code) {
                return Ok(n.node_id);
            }
            let node_id = pair(matter, crypto, ca, fab_idx, code, fabric::next_node_id(nodes)).await.map_err(|e| format!("pairing failed: {e:?}"))?;
            nodes.push(PairedNode { node_id, code: code.clone() });
            fabric::save_nodes(&config.dir, nodes).map_err(|e| format!("paired, but the node list was not kept: {e}"))?;
            return Ok(node_id);
        }
        Command::SetOnOff { node_id, .. }
        | Command::MoveToLevel { node_id, .. }
        | Command::SetSetpoint { node_id, .. }
        | Command::SetSystemMode { node_id, .. }
        | Command::SetFanMode { node_id, .. } => {
            if !known(*node_id) {
                return Err(format!("no node {node_id:#x}"));
            }
            let reader = node::Reader { matter, crypto, fab_idx, node_id: *node_id };
            reader.command(command).await.map(|()| *node_id)
        }
    };
    result.map_err(|e| format!("{e:?}"))
}
