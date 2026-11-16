//! CalendarServiceBundle: the agenda of the house. It links the panel to a Google account the way
//! Home Assistant does (Google's OAuth for a web application with PKCE; a QR code on the screen,
//! the phone does the consent, a static relay page sends the phone back to this bundle's setup
//! page on the local network), keeps the account's refresh token in its own directory, reads the
//! next seven days of the account's primary calendar every five minutes and publishes them on the
//! topic `agenda` (catalog/calendar.proto, catalog/everyware.vsidl), with the state of the link.
//! The screen gets it through the dashboard bundle. README.md has the flow and the security.
//!
//! The shape is the service bundle vsidlc generates (`vsidlc --services`): a `ServiceBundle`
//! with `SdvComms`, a current-thread runtime, `sdv_main` with the bundle's publisher; the
//! business logic is in `handle_agenda_publisher`. The parsing is in the client crate (client/),
//! which has the host test.

use everyware_bundle_store::Store;
use everyware_calendar_client::events::{self, ApiError, DAYS};
use everyware_calendar_client::oauth::{self, AuthError, ClientConfig, Flows, Grant, SetupToken, State, Tokens};
use everyware_calendar_client::setup::{self, Reject, Request, PORT};
use everyware_calendar_client::net;
use everyware_catalog::calendar::{Agenda, CalendarLink};
use log::{debug, error, info, warn};
use sdv::mw::{clientlib, Communicate, Publisher, SdvComms};
use sdv_mw_rs_com_everyware_calendar_service_bundle::{CalendarServiceBundle as Bundle, PublisherDescriptors};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

/// A calendar changes slowly; five minutes keeps a new appointment on the screen soon enough.
const POLL_INTERVAL: Duration = Duration::from_secs(300);
/// A failed read is tried again after this, then after twice as long each time, up to
/// `RETRY_MAX`: the bundle starts seconds before the network is there, so the first ones fail.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// While there is no OAuth client, the bundle looks for its file this often.
const CLIENT_CHECK: Duration = Duration::from_secs(10);
/// While the setup page is open: the panel's address read again and the token replaced when due.
const SETUP_TICK: Duration = Duration::from_secs(10);
/// The port taken by something else: tried again after this.
const BIND_RETRY: Duration = Duration::from_secs(10);
/// A request on the setup page is read and answered within this, else the connection is closed.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections served at once, at most; more are closed at once.
const MAX_CONNECTIONS: usize = 8;
/// An access token is renewed when it has less than this left.
const REFRESH_MARGIN: Duration = Duration::from_secs(120);
/// The last agenda, published again at the next start until the first new read.
const LAST_AGENDA: &str = "agenda.pb";
/// Older than this the last agenda's events are not shown.
const LAST_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
/// The owner's OAuth client (Google's client JSON): scripts/calendar-client.sh puts it there.
const CLIENT_FILE: &str = "oauth-client.json";
const MAX_CLIENT_FILE: usize = 16 * 1024;
/// The linked account's refresh token (0600).
const GRANT_FILE: &str = "grant.json";
const MAX_GRANT_FILE: usize = 4 * 1024;

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
    handle_agenda_publisher(Output { publisher, store, last: None }).await;
    Ok(())
}

/// The topic and the bundle's directory.
struct Output {
    publisher: Publisher<Agenda>,
    store: Arc<Store>,
    /// The agenda published last.
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

    /// `agenda` kept for the next start.
    fn keep(&self, agenda: &Agenda) {
        if let Err(err) = self.store.save(LAST_AGENDA, agenda) {
            warn!("the agenda not kept: {err}");
        }
    }
}

/// The link's states, one after the other: no client (wait for its file), no account (the setup
/// page), an account (the agenda), and back to the setup page when the account's grant is
/// revoked. The last agenda of the previous start goes out first, when it is a day old at most.
async fn handle_agenda_publisher(mut out: Output) {
    // an EXPIRED of the last start is still expired, however old: the account was not linked again
    let mut expired = out.store.load::<Agenda>(LAST_AGENDA, Duration::MAX).is_some_and(|a| link_of(&a) == CalendarLink::CALENDAR_EXPIRED);
    let mut kept = out.store.load::<Agenda>(LAST_AGENDA, LAST_MAX_AGE).filter(|a| link_of(a) == CalendarLink::CALENDAR_CONNECTED);
    let mut no_client = String::new();
    loop {
        let client = match load_client(&out.store) {
            Ok(client) => client,
            Err(why) => {
                if why != no_client {
                    warn!("no OAuth client: {why}; looking for {CLIENT_FILE} every {} s", CLIENT_CHECK.as_secs());
                    no_client = why;
                }
                out.publish(agenda_of(CalendarLink::CALENDAR_NO_CLIENT));
                sleep(CLIENT_CHECK).await;
                continue;
            }
        };
        no_client.clear();
        info!("OAuth client {}", client.client_id);
        let (grant, tokens) = match load_grant(&out.store, &client) {
            Some(grant) => {
                match kept.take() {
                    Some(last) => {
                        info!("the last agenda: {} events", last.events.len());
                        out.publish(last);
                    }
                    None => out.publish(agenda_of(CalendarLink::CALENDAR_CONNECTED)),
                }
                (grant, None)
            }
            None => {
                let (grant, tokens) = run_setup(&client, &mut out, expired).await;
                if let Err(err) = out.store.save_private(GRANT_FILE, grant.to_json().as_bytes()) {
                    error!("the grant not kept, the account has to be linked again after a restart: {err}");
                }
                out.publish(agenda_of(CalendarLink::CALENDAR_CONNECTED));
                (grant, Some(tokens))
            }
        };
        read_agenda(&client, grant, tokens, &mut out).await;
        // the grant is not good any more: forgotten, and the events with it
        if let Err(err) = out.store.remove(GRANT_FILE) {
            error!("the grant not removed: {err}");
        }
        expired = true;
        let agenda = agenda_of(CalendarLink::CALENDAR_EXPIRED);
        out.keep(&agenda);
        out.publish(agenda);
    }
}

/// The link's state of an agenda.
fn link_of(agenda: &Agenda) -> CalendarLink {
    agenda.link.enum_value_or_default()
}

/// An agenda with `link` and nothing else.
fn agenda_of(link: CalendarLink) -> Agenda {
    let mut agenda = Agenda::new();
    agenda.link = link.into();
    agenda
}

/// The owner's OAuth client, or why there is none.
fn load_client(store: &Store) -> Result<ClientConfig, String> {
    let bytes = store.read(CLIENT_FILE, MAX_CLIENT_FILE).ok_or_else(|| format!("no {CLIENT_FILE} (or over {MAX_CLIENT_FILE} bytes)"))?;
    ClientConfig::parse(&String::from_utf8_lossy(&bytes)).map_err(|why| format!("{CLIENT_FILE}: {why}"))
}

/// The kept grant, when it is one of `client`'s.
fn load_grant(store: &Store, client: &ClientConfig) -> Option<Grant> {
    let grant = Grant::parse(&store.read(GRANT_FILE, MAX_GRANT_FILE)?)?;
    if grant.client_id != client.client_id {
        warn!("the kept grant is of another OAuth client: the account has to be linked again");
        return None;
    }
    Some(grant)
}

/// Unix time now.
fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

// The setup page

/// What the setup page's connections share.
struct Setup {
    client: ClientConfig,
    state: Mutex<SetupState>,
    /// A linked account, to the setup loop.
    linked: mpsc::Sender<(Grant, Tokens)>,
    /// The token of the QR code was used: its URL changes.
    used: mpsc::UnboundedSender<()>,
}

struct SetupState {
    token: SetupToken,
    flows: Flows,
}

/// Until an account is linked: the setup page on port 8765 of every interface, and its URL in
/// the agenda (a new one after every use and every ten minutes, and when the panel's address
/// changes). Returns the linked account; the page closes with it.
async fn run_setup(client: &ClientConfig, out: &mut Output, expired: bool) -> (Grant, Tokens) {
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
    let (linked, mut linked_rx) = mpsc::channel(1);
    let (used, mut used_rx) = mpsc::unbounded_channel();
    let setup = Arc::new(Setup {
        client: client.clone(),
        state: Mutex::new(SetupState { token: SetupToken::new(Instant::now()), flows: Flows::default() }),
        linked,
        used,
    });
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut tick = tokio::time::interval(SETUP_TICK);
    let mut address = None;
    loop {
        tokio::select! {
            Some(account) = linked_rx.recv() => {
                info!("the account is linked, the setup page closes");
                return account;
            }
            Some(()) = used_rx.recv() => {}
            _ = tick.tick() => {
                let now = net::read_lan_address();
                if now != address {
                    info!("the setup page: {}", now.map_or("no address on the network yet".to_string(), |a| format!("open on {a}:{PORT}")));
                    address = now;
                }
                setup.state.lock().expect("setup state").token.rotate_if_due(Instant::now());
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => match connections.clone().try_acquire_owned() {
                    Ok(permit) => {
                        tokio::spawn(serve(stream, permit, setup.clone()));
                    }
                    Err(_) => debug!("{peer}: {MAX_CONNECTIONS} connections already, closed"),
                },
                Err(err) => {
                    // out of file descriptors, for one: a moment for the others to finish
                    warn!("the setup page: {err}");
                    sleep(Duration::from_millis(200)).await;
                }
            },
        }
        let url = address.map(|a| format!("http://{a}:{PORT}/setup?t={}", setup.state.lock().expect("setup state").token.value()));
        let mut agenda = agenda_of(link);
        agenda.setup_url = url.unwrap_or_default();
        if out.last.as_ref().map(|l| &l.setup_url) != Some(&agenda.setup_url) {
            // the URL carries the one-time token: a debug line only
            debug!("setup URL {}", agenda.setup_url);
        }
        out.publish(agenda);
    }
}

/// One connection to the setup page: its request read within `REQUEST_TIMEOUT`, its answer
/// written, the connection closed. (Closed by dropping it: shutdown() is not among what the
/// bundle's SELinux domain may do on a socket.)
async fn serve(mut stream: TcpStream, _permit: OwnedSemaphorePermit, setup: Arc<Setup>) {
    let answer = match timeout(REQUEST_TIMEOUT, read_head(&mut stream)).await {
        Err(_) => return,
        Ok(Err(why)) => setup::reject(why),
        Ok(Ok(head)) => match setup::parse(&head) {
            Ok(request) => answer(request, stream.local_addr().ok(), &setup).await,
            Err(why) => setup::reject(why),
        },
    };
    let _ = timeout(REQUEST_TIMEOUT, stream.write_all(&answer)).await;
}

/// A request's head, up to its blank line, at most `setup::MAX_REQUEST` bytes.
async fn read_head(stream: &mut TcpStream) -> Result<Vec<u8>, Reject> {
    let mut head = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await.map_err(|_| Reject::BadRequest)?;
        if n == 0 {
            return Err(Reject::BadRequest);
        }
        head.extend_from_slice(&chunk[..n]);
        if let Some(end) = setup::head_end(&head) {
            head.truncate(end);
            return Ok(head);
        }
        if head.len() > setup::MAX_REQUEST {
            return Err(Reject::TooLarge);
        }
    }
}

/// The answer to a request of the setup page. `local`: the address the browser reached the
/// panel on, where Google's answer comes back through the relay page.
async fn answer(request: Request, local: Option<SocketAddr>, setup: &Setup) -> Vec<u8> {
    const AGAIN: &str = "Scan the code on the panel to try again.";
    match request {
        Request::Setup { token } => {
            let Some(SocketAddr::V4(local)) = local else {
                return setup::page("500 Internal Server Error", "The panel does not know its address.");
            };
            let url = {
                let mut state = setup.state.lock().expect("setup state");
                let now = Instant::now();
                if !state.token.take(&token, now) {
                    return setup::page("403 Forbidden", &format!("This link is old or was used. {AGAIN}"));
                }
                let (nonce, pkce) = state.flows.begin(now);
                let flow = State { panel: format!("{}:{PORT}", local.ip()), nonce };
                oauth::auth_url(&setup.client, &pkce.challenge, &flow.encode())
            };
            let _ = setup.used.send(());
            info!("the setup page: a link begins, to Google's consent screen");
            setup::redirect(&url)
        }
        Request::Denied { error, state } => {
            if let Some(flow) = State::decode(&state) {
                setup.state.lock().expect("setup state").flows.take(&flow.nonce, Instant::now());
            }
            info!("the setup page: Google answered {error}");
            setup::page("200 OK", &format!("The calendar was not linked ({error}). {AGAIN}"))
        }
        Request::Callback { code, state } => {
            let verifier = State::decode(&state).and_then(|flow| setup.state.lock().expect("setup state").flows.take(&flow.nonce, Instant::now()));
            let Some(verifier) = verifier else {
                info!("the setup page: an answer of no open flow");
                return setup::page("400 Bad Request", &format!("This sign-in is old, was used or is not of this panel. {AGAIN}"));
            };
            match oauth::exchange(&setup.client, &code, &verifier).await {
                Ok(tokens) => {
                    let Some(refresh_token) = tokens.refresh_token.clone() else {
                        warn!("the setup page: Google sent no refresh token");
                        return setup::page("502 Bad Gateway", &format!("Google sent no lasting access. {AGAIN}"));
                    };
                    let grant = Grant { client_id: setup.client.client_id.clone(), refresh_token };
                    match setup.linked.try_send((grant, tokens)) {
                        Ok(()) => setup::page("200 OK", "Done. You can close this page."),
                        Err(_) => setup::page("409 Conflict", "The panel was linked by another sign-in just now."),
                    }
                }
                Err(AuthError::Scope) => {
                    info!("the setup page: the calendar permission was not granted");
                    setup::page("403 Forbidden", &format!("The calendar permission was not granted. {AGAIN}"))
                }
                Err(err) => {
                    warn!("the setup page: the code exchange failed: {err}");
                    setup::page("502 Bad Gateway", &format!("Google did not complete the link. {AGAIN}"))
                }
            }
        }
    }
}

// The agenda

/// The access token and until when it is good.
struct Access {
    token: String,
    until: Instant,
}

/// Why a read did not give the agenda.
enum Failure {
    /// The grant is not good any more: link the account again.
    Revoked(String),
    /// Anything else: try again later.
    Again(String),
}

/// The agenda of the linked account: read at once, then every `POLL_INTERVAL`; a failed read is
/// tried again sooner. Returns when the account's grant is not good any more.
async fn read_agenda(client: &ClientConfig, mut grant: Grant, tokens: Option<Tokens>, out: &mut Output) {
    let mut access = tokens.map(|t| Access { until: Instant::now() + Duration::from_secs(t.expires_in), token: t.access_token });
    let mut retry = RETRY_FIRST;
    info!("reading the calendar every {} s", POLL_INTERVAL.as_secs());
    loop {
        match read_once(client, &mut grant, &mut access, &out.store).await {
            Ok(read) => {
                info!("{} events in the next {DAYS} days", read.events.len());
                let mut agenda = agenda_of(CalendarLink::CALENDAR_CONNECTED);
                agenda.calendar = read.calendar;
                agenda.events = read.events;
                agenda.updated = unix_now();
                out.keep(&agenda);
                out.publish(agenda);
                retry = RETRY_FIRST;
                sleep(POLL_INTERVAL).await;
            }
            Err(Failure::Revoked(why)) => {
                warn!("the account's grant is not good any more ({why}): the setup page again");
                return;
            }
            Err(Failure::Again(why)) => {
                warn!("reading the calendar failed: {why}; again in {} s", retry.as_secs());
                sleep(retry).await;
                retry = (retry * 2).min(RETRY_MAX);
            }
        }
    }
}

/// One read: the access token renewed when it is due, the events, and once more with a new
/// access token when the API refused the one it had.
async fn read_once(client: &ClientConfig, grant: &mut Grant, access: &mut Option<Access>, store: &Store) -> Result<events::Read, Failure> {
    for attempt in 0..2 {
        if access.as_ref().is_none_or(|a| Instant::now() + REFRESH_MARGIN >= a.until) {
            *access = Some(renew(client, grant, store).await?);
        }
        let token = access.as_ref().map(|a| a.token.clone()).unwrap_or_default();
        match events::read(&token, unix_now()).await {
            Ok(read) => return Ok(read),
            Err(ApiError::Unauthorized) if attempt == 0 => *access = None,
            Err(err) => return Err(Failure::Again(err.to_string())),
        }
    }
    Err(Failure::Again("the access token was refused twice".into()))
}

/// A new access token with the grant's refresh token; a new refresh token, when Google sends
/// one, kept in place of the old.
async fn renew(client: &ClientConfig, grant: &mut Grant, store: &Store) -> Result<Access, Failure> {
    let tokens = oauth::refresh(client, &grant.refresh_token).await.map_err(|err| match err {
        AuthError::Grant(_) | AuthError::Scope => Failure::Revoked(err.to_string()),
        AuthError::Other(_) => Failure::Again(err.to_string()),
    })?;
    if let Some(refresh_token) = tokens.refresh_token.filter(|t| *t != grant.refresh_token) {
        grant.refresh_token = refresh_token;
        if let Err(err) = store.save_private(GRANT_FILE, grant.to_json().as_bytes()) {
            error!("the new refresh token not kept: {err}");
        }
    }
    debug!("a new access token for {} s", tokens.expires_in);
    Ok(Access { until: Instant::now() + Duration::from_secs(tokens.expires_in), token: tokens.access_token })
}
