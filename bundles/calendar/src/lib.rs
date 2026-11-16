// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! CalendarServiceBundle: the agenda of the house. It links the panel to a Google account the way
//! Home Assistant does (Google's OAuth for a web application with PKCE; a QR code on the screen,
//! the phone does the consent, a static relay page sends the phone back to this bundle's setup
//! page on the local network), keeps the account's refresh token in its own directory, reads the
//! next seven days of the account's primary calendar every five minutes and publishes them on the
//! topic `agenda` (catalog/calendar.proto, catalog/everyware.vsidl), with the state of the link.
//! The screen gets it through the dashboard bundle. README.md has the flow and the security.
//!
//! The shape is the service bundle vsidlc generates (`vsidlc --services`): a `ServiceBundle`
//! with `SdvComms`, a current-thread runtime, `sdv_main` with the bundle's publisher. This file
//! only loops, sleeps and publishes (`handle_agenda_publisher`); every decision is in the client
//! crate (client/: `link` the reads and their outcomes, `server` the setup page), which has the
//! host test.

use everyware_bundle_store::Store;
use everyware_calendar_client::events::DAYS;
use everyware_calendar_client::google::Live;
use everyware_calendar_client::link::{agenda_of, Link, Outcome, Session, CLIENT_FILE};
use everyware_calendar_client::net;
use everyware_calendar_client::oauth::{ClientConfig, Grant, Tokens};
use everyware_calendar_client::server::{Limits, SetupEnd, SetupPage};
use everyware_calendar_client::setup::PORT;
use everyware_catalog::calendar::{Agenda, CalendarLink};
use log::{debug, error, info, warn};
use sdv::mw::{clientlib, Communicate, Publisher, SdvComms};
use sdv_mw_rs_com_everyware_calendar_service_bundle::{CalendarServiceBundle as Bundle, PublisherDescriptors};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

/// A calendar changes slowly; five minutes keeps a new appointment on the screen soon enough.
const POLL_INTERVAL: Duration = Duration::from_secs(300);
/// A failed read is tried again after this, then after twice as long each time, up to
/// `RETRY_MAX`: the bundle starts seconds before the network is there, so the first ones fail.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// While there is no usable client, the bundle looks at its file this often.
const CLIENT_CHECK: Duration = Duration::from_secs(10);
/// While the setup page is open: the panel's address read again and the token replaced when due.
const SETUP_TICK: Duration = Duration::from_secs(10);
/// The port taken by something else: tried again after this.
const BIND_RETRY: Duration = Duration::from_secs(10);

sdv::lifecycle::register_service_bundle!(CalendarServiceBundle);

struct CalendarServiceBundle {
    cancellation_token: Option<CancellationToken>,
    context: ContextRef,
    /// The bundle's directory (/data/service_bundle_data/<package>/<bundle>).
    store: Arc<Store>,
}

impl ServiceBundle for CalendarServiceBundle {
    fn new(context: ContextRef) -> Self {
        if let Err(err) = sdv::tracing::try_init_tracing() {
            warn!("Failed to init service bundles tracing: {err}");
        }
        let fqin = context.get_self_fqin();
        let store = Arc::new(Store::new(fqin.get_sdv_package_name(), fqin.get_service_bundle_name()));
        Self { cancellation_token: None, context, store }
    }

    fn on_start(&mut self) {
        // The runner has set up the logger already, with log_tag from the bundle manifest.
        let context = self.context;
        let store = self.store.clone();
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
                    res = sdv_main(comms, store) => res,
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

async fn sdv_main(comms: Arc<dyn Communicate>, store: Arc<Store>) -> sdv::status::SdvResult<()> {
    info!("Starting service bundle 'CalendarServiceBundle'");
    let _service_bundle = match Bundle::builder().set_comms(comms.clone()).build().await {
        Ok(service) => service,
        Err(e) => {
            error!("Failed to build service bundle: {e}");
            return Err(sdv::status::SdvStatus::with_message(sdv::status::SdvStatusCode::Internal, e.to_string()));
        }
    };
    let publisher = clientlib::create_publisher(comms.as_ref(), PublisherDescriptors::<Agenda>::AGENDA).await.map_err(|e| {
        error!("Failed to create publisher Agenda#agenda: {e:?}");
        e
    })?;
    handle_agenda_publisher(Output { publisher, last: None }, Link::new(store)).await;
    Ok(())
}

/// The topic, and the agenda published last.
struct Output {
    publisher: Publisher<Agenda>,
    last: Option<Agenda>,
}

impl Output {
    /// Publishes `agenda` unless it is the one published last.
    fn publish(&mut self, agenda: Agenda) {
        if self.last.as_ref() == Some(&agenda) {
            return;
        }
        match self.publisher.publish(&agenda) {
            Ok(()) => self.last = Some(agenda),
            Err(e) => error!("Failed to publish on Agenda#agenda: {e:?}"),
        }
    }
}

/// Unix time now.
fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The link's states, one after the other (client/src/link.rs has the table): no usable client
/// (look at its file every 10 s), no account (the setup page), an account (the agenda), and back:
/// to the setup page when the grant is revoked, to no client when Google refuses the client. The
/// last agenda of the previous start goes out first, with a grant and when it is a day old at most.
async fn handle_agenda_publisher(mut out: Output, mut link: Link) {
    let (mut expired, mut kept) = link.last();
    let mut no_client = String::new();
    loop {
        let client = match link.client() {
            Ok(client) => client,
            Err(why) => {
                if why != no_client {
                    warn!("no OAuth client: {why}; looking at {CLIENT_FILE} every {} s", CLIENT_CHECK.as_secs());
                    no_client = why;
                }
                out.publish(agenda_of(CalendarLink::CALENDAR_NO_CLIENT));
                sleep(CLIENT_CHECK).await;
                continue;
            }
        };
        no_client.clear();
        info!("OAuth client {}", client.client_id);
        let session = match link.grant(&client) {
            Some(grant) => {
                match kept.take() {
                    Some(last) => {
                        info!("the last agenda: {} events", last.events.len());
                        out.publish(last);
                    }
                    // connected, not read yet: updated 0
                    None => out.publish(agenda_of(CalendarLink::CALENDAR_CONNECTED)),
                }
                Session::new(client, grant, None, Instant::now())
            }
            None => match run_setup(&client, &mut out, expired).await {
                SetupEnd::Linked(grant, tokens) => {
                    expired = false;
                    linked(&link, &mut out, client, grant, tokens)
                }
                SetupEnd::Refused(why) => {
                    warn!("{why}: no client until {CLIENT_FILE} changes");
                    out.publish(link.refused());
                    continue;
                }
            },
        };
        match read_agenda(session, &link, &mut out).await {
            Ended::Revoked(why) => {
                warn!("the account's grant is not good any more ({why}): the setup page again");
                expired = true;
                out.publish(link.revoked());
            }
            Ended::Refused(why) => {
                warn!("{why}: no client until {CLIENT_FILE} changes");
                out.publish(link.refused());
            }
        }
    }
}

/// An account linked just now: its grant kept, connected, read next.
fn linked(link: &Link, out: &mut Output, client: ClientConfig, grant: Grant, tokens: Tokens) -> Session {
    out.publish(link.linked(&grant));
    Session::new(client, grant, Some(tokens), Instant::now())
}

/// Until an account is linked or Google refuses the client: the setup page on port 8765 of every
/// interface, and its URL in the agenda (a new one after every use and every ten minutes, and
/// when the panel's address changes). The page closes when this returns.
async fn run_setup(client: &ClientConfig, out: &mut Output, expired: bool) -> SetupEnd {
    let link = if expired { CalendarLink::CALENDAR_EXPIRED } else { CalendarLink::CALENDAR_NOT_CONNECTED };
    let listener = loop {
        match TcpListener::bind((Ipv4Addr::UNSPECIFIED, PORT)).await {
            Ok(listener) => break listener,
            Err(err) => {
                warn!("the setup page cannot listen on port {PORT}: {err}; again in {} s", BIND_RETRY.as_secs());
                out.publish(agenda_of(link));
                sleep(BIND_RETRY).await;
            }
        }
    };
    let (page, mut events) = SetupPage::start(Live, client.clone(), Limits::default(), Instant::now());
    let mut tick = tokio::time::interval(SETUP_TICK);
    let mut address = None;
    loop {
        tokio::select! {
            Some(end) = events.end.recv() => {
                if matches!(end, SetupEnd::Linked(..)) {
                    info!("the account is linked, the setup page closes");
                }
                return end;
            }
            Some(()) = events.used.recv() => {}
            _ = tick.tick() => {
                let now = net::read_lan_address();
                if now != address {
                    info!("the setup page: {}", now.map_or("no address on the network yet".to_string(), |a| format!("open on {a}:{PORT}")));
                    address = now;
                }
                page.rotate_if_due(Instant::now());
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    if !page.accept(stream) {
                        debug!("{peer}: the setup page serves enough connections, closed");
                    }
                }
                Err(err) => {
                    // out of file descriptors, for one: a moment for the others to finish
                    warn!("the setup page: {err}");
                    sleep(Duration::from_millis(200)).await;
                }
            },
        }
        let mut agenda = agenda_of(link);
        agenda.setup_url = address.map(|a| page.url(a)).unwrap_or_default();
        if out.last.as_ref().map(|l| &l.setup_url) != Some(&agenda.setup_url) {
            // the URL carries the one-time token: a debug line only
            debug!("setup URL {}", agenda.setup_url);
        }
        out.publish(agenda);
    }
}

/// Why the reads of an account ended.
enum Ended {
    Revoked(String),
    Refused(String),
}

/// The agenda of the linked account: read at once, then every `POLL_INTERVAL`; a failed read is
/// tried again sooner. Returns when the grant is not good any more or Google refuses the client.
async fn read_agenda(mut session: Session, link: &Link, out: &mut Output) -> Ended {
    let mut retry = RETRY_FIRST;
    info!("reading the calendar every {} s", POLL_INTERVAL.as_secs());
    loop {
        match session.read(&Live, link.store(), Instant::now(), unix_now()).await {
            Outcome::Read(read) => {
                info!("{} events in the next {DAYS} days", read.events.len());
                out.publish(link.connected(read, unix_now()));
                retry = RETRY_FIRST;
                sleep(POLL_INTERVAL).await;
            }
            Outcome::Revoked(why) => return Ended::Revoked(why),
            Outcome::Refused(why) => return Ended::Refused(why),
            Outcome::Again(why) => {
                warn!("reading the calendar failed: {why}; again in {} s", retry.as_secs());
                sleep(retry).await;
                retry = (retry * 2).min(RETRY_MAX);
            }
        }
    }
}
