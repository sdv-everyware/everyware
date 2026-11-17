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
//! next seven days of the account's primary calendar every minute and publishes them on the
//! topic `agenda` (catalog/calendar.proto, catalog/everyware.vsidl), with the state of the link.
//! The screen gets it through the dashboard bundle, and asks it through the CalendarService
//! (catalog/calendar_service.proto): with write access granted an answer to an invitation or the
//! deletion of the account's own event, and the unlink of the account (also how the access
//! changes). README.md has the flow and the security.
//!
//! The shape is the service bundle vsidlc generates (`vsidlc --services`): a `ServiceBundle`
//! with `SdvComms`, a current-thread runtime, `sdv_main` with the bundle's publisher and its RPC
//! server. This file only loops, sleeps, publishes and passes the screen's changes into the loop
//! (`handle_agenda_publisher`); every decision is in the client crate (client/: `link` the reads
//! and their outcomes, `change` the changes, `server` the setup page), which has the host test.

use everyware_bundle_store::Store;
use everyware_calendar_client::change::{self, Change, Failure};
use everyware_calendar_client::events::DAYS;
use everyware_calendar_client::google::{Google, Live};
use everyware_calendar_client::link::{agenda_of, failed, not_read, Link, Outcome, Pace, Session, CLIENT_FILE, POLL_INTERVAL, SETUP_URL_FILE};
use everyware_calendar_client::net;
use everyware_calendar_client::oauth::{ClientConfig, Grant, Tokens};
use everyware_calendar_client::server::{Limits, SetupEnd, SetupPage};
use everyware_calendar_client::setup::PORT;
use everyware_catalog::calendar::{Agenda, CalendarLink};
use everyware_catalog::calendar_service::{CalendarDelete, CalendarDisconnect, CalendarFailure, CalendarRespond, CalendarResult};
use log::{debug, error, info, warn};
use sdv::comms::id::ServiceFqin;
use sdv::mw::{clientlib, Communicate, Publisher, SdvComms};
use sdv::status::{SdvResult, SdvStatus, SdvStatusCode};
use sdv_mw_rs_com_everyware_calendar_service_bundle::{CalendarServiceBundle as Bundle, PublisherDescriptors};
use std::future::Future;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, sleep_until, timeout};
use tokio_util::sync::CancellationToken;

/// While there is no usable client, the bundle looks at its file this often.
const CLIENT_CHECK: Duration = Duration::from_secs(10);
/// While the setup page is open: the panel's address read again and the token replaced when due.
const SETUP_TICK: Duration = Duration::from_secs(10);
/// The port taken by something else: tried again after this.
const BIND_RETRY: Duration = Duration::from_secs(10);
/// Changes of the screen waiting for the loop, at most: a screen sends one press at a time.
const CHANGES_QUEUED: usize = 4;
/// A change's calls to Google (a renewal of the token, the read of the event, the change, once
/// more after a 401 or a 412), at most; the read of the agenda after it comes on top.
const CHANGE_WORK: Duration = Duration::from_secs(40);
/// A change is answered within this, or "no answer in time"; the dashboard bundle waits longer.
const CHANGE_ANSWER: Duration = Duration::from_secs(80);

/// What the screen asks of the loop, and where its answer goes.
enum Asked {
    /// A change of an event.
    Change(Change, oneshot::Sender<CalendarResult>),
    /// The unlink of the account.
    Disconnect(oneshot::Sender<CalendarResult>),
}

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
    let (changes_tx, changes) = mpsc::channel(CHANGES_QUEUED);
    let rpc = Arc::new(CalendarServiceRpc { changes: changes_tx });
    let _service_bundle = match Bundle::builder().set_comms(comms.clone()).set_server_calendar(rpc).build().await {
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
    info!("serving CalendarService on the channel calendar");
    handle_agenda_publisher(Output { publisher, last: None }, Link::new(store), changes).await;
    Ok(())
}

/// The CalendarService server: what the screen asks, checked, into the loop; the loop answers
/// once it is done (or not) and the agenda after it is out.
struct CalendarServiceRpc {
    changes: mpsc::Sender<Asked>,
}

impl CalendarServiceRpc {
    /// The authz policy lets any bundle that declares itself a client call; only the screen's
    /// bridge asks.
    fn check_caller(caller: &ServiceFqin) -> SdvResult<()> {
        if change::caller_allowed(caller.get_sdv_package_name(), caller.get_service_bundle_name()) {
            Ok(())
        } else {
            warn!("a request from {caller}: not the dashboard bundle, refused");
            Err(SdvStatus::new(SdvStatusCode::PermissionDenied))
        }
    }

    /// A change of an event, well formed, into the loop.
    async fn change(&self, caller: &ServiceFqin, change: Result<Change, Failure>) -> SdvResult<CalendarResult> {
        Self::check_caller(caller)?;
        match change {
            Ok(change) => Ok(self.pass(|reply| Asked::Change(change, reply)).await),
            Err(failure) => {
                warn!("a change that is not one: {}", failure.why);
                Ok(change::result(&Err(failure)))
            }
        }
    }

    /// Into the loop, and its answer.
    async fn pass(&self, asked: impl FnOnce(oneshot::Sender<CalendarResult>) -> Asked) -> CalendarResult {
        let (reply, answer) = oneshot::channel();
        if self.changes.try_send(asked(reply)).is_err() {
            return change::result(&Err(Failure::again("the calendar is busy with other changes")));
        }
        match timeout(CHANGE_ANSWER, answer).await {
            Ok(Ok(result)) => result,
            _ => change::result(&Err(Failure::again("no answer in time"))),
        }
    }
}

#[async_trait::async_trait]
#[allow(non_snake_case)]
impl com_everyware_calendar_service_rpc::Interface for CalendarServiceRpc {
    async fn Respond(&self, caller_id: ServiceFqin, request: &CalendarRespond) -> SdvResult<CalendarResult> {
        self.change(&caller_id, Change::respond(request)).await
    }

    async fn Delete(&self, caller_id: ServiceFqin, request: &CalendarDelete) -> SdvResult<CalendarResult> {
        self.change(&caller_id, Change::delete(request)).await
    }

    async fn Disconnect(&self, caller_id: ServiceFqin, _request: &CalendarDisconnect) -> SdvResult<CalendarResult> {
        Self::check_caller(&caller_id)?;
        Ok(self.pass(Asked::Disconnect).await)
    }
}

/// While no account is read: waits for `wait`, and what the screen asks meanwhile is answered at
/// once that no account is linked.
async fn refuse_while(changes: &mut mpsc::Receiver<Asked>, wait: impl Future<Output = ()>) {
    tokio::pin!(wait);
    loop {
        tokio::select! {
            () = &mut wait => return,
            Some(asked) = changes.recv() => refuse(asked),
        }
    }
}

/// A change or an unlink while no account is linked.
fn refuse(asked: Asked) {
    let (Asked::Change(_, reply) | Asked::Disconnect(reply)) = asked;
    let _ = reply.send(change::result(&Err(Failure::new(CalendarFailure::CALENDAR_FAILURE_NOT_LINKED, "no account is linked"))));
}

/// The grant of a client the owner replaced: useless with the new one, still good at Google, so
/// it is ended there and forgotten. When Google cannot be told it stays, for the next start.
async fn end_replaced(link: &Link, client: &ClientConfig) {
    let Some(old) = link.grant_of_another_client(client) else { return };
    match Live.revoke(&old.refresh_token).await {
        Ok(()) => {
            info!("the grant of the replaced OAuth client is ended at Google");
            link.forget_grant();
        }
        Err(err) => warn!("the grant of the replaced OAuth client could not be ended at Google ({err}); again at the next start"),
    }
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
/// A change of the screen is made while an account is read, and refused in the other states.
async fn handle_agenda_publisher(mut out: Output, mut link: Link, mut changes: mpsc::Receiver<Asked>) {
    let (mut expired, mut kept) = link.last();
    // a setup URL a stopped bundle left holds a token of no page any more
    link.setup_url("");
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
                refuse_while(&mut changes, sleep(CLIENT_CHECK)).await;
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
                    None => out.publish(not_read(&grant)),
                }
                Session::new(client, grant, None, Instant::now())
            }
            None => {
                end_replaced(&link, &client).await;
                match run_setup(&client, &link, &mut out, expired, &mut changes).await {
                    SetupEnd::Linked(grant, tokens) => {
                        expired = false;
                        linked(&link, &mut out, client, grant, tokens)
                    }
                    SetupEnd::Refused(why) => {
                        warn!("{why}: no client until {CLIENT_FILE} changes");
                        out.publish(link.refused());
                        continue;
                    }
                }
            }
        };
        match read_agenda(session, &mut link, &mut out, &mut changes).await {
            Ended::Revoked(why) => {
                warn!("the account's grant is not good any more ({why}): the setup page again");
                expired = true;
                out.publish(link.revoked());
            }
            Ended::Refused(why) => {
                warn!("{why}: no client until {CLIENT_FILE} changes");
                out.publish(link.refused());
            }
            // the setup page again, with the choice of access
            Ended::Disconnected => expired = false,
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
/// when the panel's address changes). The page closes when this returns. A change of the screen
/// is refused meanwhile.
async fn run_setup(client: &ClientConfig, files: &Link, out: &mut Output, expired: bool, changes: &mut mpsc::Receiver<Asked>) -> SetupEnd {
    let link = if expired { CalendarLink::CALENDAR_EXPIRED } else { CalendarLink::CALENDAR_NOT_CONNECTED };
    let listener = loop {
        match TcpListener::bind((Ipv4Addr::UNSPECIFIED, PORT)).await {
            Ok(listener) => break listener,
            Err(err) => {
                warn!("the setup page cannot listen on port {PORT}: {err}; again in {} s", BIND_RETRY.as_secs());
                out.publish(agenda_of(link));
                refuse_while(changes, sleep(BIND_RETRY)).await;
            }
        }
    };
    let (page, mut events) = SetupPage::start(Live, client.clone(), Limits::default(), Instant::now());
    info!("the setup URL, for development: adb root, then cat <the bundle's directory>/{SETUP_URL_FILE}");
    let mut tick = tokio::time::interval(SETUP_TICK);
    let mut address = None;
    loop {
        tokio::select! {
            Some(end) = events.end.recv() => {
                if matches!(end, SetupEnd::Linked(..)) {
                    info!("the account is linked, the setup page closes");
                }
                files.setup_url("");
                return end;
            }
            Some(()) = events.used.recv() => {}
            Some(asked) = changes.recv() => refuse(asked),
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
            // the URL carries the one-time token: not in the log, in a 0600 file for development
            files.setup_url(&agenda.setup_url);
        }
        out.publish(agenda);
    }
}

/// Why the reads of an account ended.
enum Ended {
    Revoked(String),
    Refused(String),
    /// The screen unlinked the account; its grant is ended at Google and forgotten.
    Disconnected,
}

/// The agenda of the linked account: read at once, then every `POLL_INTERVAL`; after a failed
/// read the waits grow (`Pace`), and once the reads have failed for `FAILED_AFTER` the last
/// agenda goes out again with `read_failed`. A change of the screen is made between two reads;
/// when it reached Google the agenda is read at once, and the answer goes back once that agenda is
/// out. Returns when the grant is not good any more, Google refuses the client, or the screen
/// unlinked the account.
async fn read_agenda(mut session: Session, link: &mut Link, out: &mut Output, changes: &mut mpsc::Receiver<Asked>) -> Ended {
    let mut pace = Pace::default();
    let mut count = None;
    // the answer to a change, sent after the read that follows it
    let mut answer: Option<(oneshot::Sender<CalendarResult>, CalendarResult)> = None;
    info!("reading the calendar every {} s", POLL_INTERVAL.as_secs());
    loop {
        let wait = match session.read(&Live, link.store(), Instant::now(), unix_now()).await {
            Outcome::Read(read) => {
                // a line a minute only when the count changes
                if count != Some(read.events.len()) {
                    info!("{} events in the next {DAYS} days", read.events.len());
                    count = Some(read.events.len());
                }
                out.publish(link.connected(read, session.can_change(), unix_now(), Instant::now()));
                pace.after_read()
            }
            Outcome::Revoked(why) => {
                send(answer);
                return Ended::Revoked(why);
            }
            Outcome::Refused(why) => {
                send(answer);
                return Ended::Refused(why);
            }
            Outcome::Again(why) => {
                let wait = pace.after_failure();
                warn!("reading the calendar failed: {why}; again in {} s", wait.as_secs());
                if let Some(agenda) = failed(out.last.as_ref()).filter(|_| pace.failing()) {
                    out.publish(agenda);
                }
                wait
            }
        };
        send(answer.take());
        let next = tokio::time::Instant::now() + wait;
        loop {
            tokio::select! {
                () = sleep_until(next) => break,
                Some(asked) = changes.recv() => match asked {
                    Asked::Change(change, reply) => {
                        let (result, read_now) = make(&mut session, link, out.last.as_ref(), &change).await;
                        if read_now {
                            answer = Some((reply, result));
                            break;
                        }
                        let _ = reply.send(result);
                    }
                    Asked::Disconnect(reply) => {
                        let outcome = timeout(CHANGE_WORK, session.disconnect(&Live)).await.unwrap_or_else(|_| Err(Failure::again("Google did not answer in time")));
                        let unlinked = outcome.is_ok();
                        match &outcome {
                            Ok(()) => {
                                info!("the account is unlinked: its grant is ended at Google and forgotten");
                                out.publish(link.disconnected());
                            }
                            Err(failure) => warn!("the account is not unlinked: {}", failure.why),
                        }
                        let _ = reply.send(change::result(&outcome));
                        if unlinked {
                            return Ended::Disconnected;
                        }
                    }
                },
            }
        }
    }
}

/// The answer to a change, if one waits.
fn send(answer: Option<(oneshot::Sender<CalendarResult>, CalendarResult)>) {
    if let Some((reply, result)) = answer {
        let _ = reply.send(result);
    }
}

/// One change of the screen: the agenda published last decides first, then Google's event
/// (`Session::change`, within `CHANGE_WORK`). The answer, and whether the agenda is to be read
/// at once: when the change was made, or Google said something about the event or the access
/// that the agenda should show (gone, not allowed any more, read only, refused).
async fn make(session: &mut Session, link: &Link, last: Option<&Agenda>, change: &Change) -> (CalendarResult, bool) {
    if let Err(failure) = change.allowed_by(last) {
        info!("{change:?}: not made, {}", failure.why);
        return (change::result(&Err(failure)), false);
    }
    let outcome = match timeout(CHANGE_WORK, session.change(&Live, link.store(), change, Instant::now())).await {
        Ok(outcome) => outcome,
        Err(_) => Err(Failure::again("Google did not answer in time")),
    };
    match &outcome {
        Ok(()) => info!("{change:?}: made"),
        Err(failure) => warn!("{change:?}: not made, {:?}: {}", failure.kind, failure.why),
    }
    let read_now = outcome.as_ref().err().is_none_or(|f| f.kind != CalendarFailure::CALENDAR_FAILURE_AGAIN);
    (change::result(&outcome), read_now)
}
