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

//! The setup page's server side, while no account is linked: a connection is read within
//! `Limits::request_timeout`, at most `setup::MAX_REQUEST` bytes of it, answered and closed, and at
//! most `Limits::max_connections` are served at a time (one more is closed at once). Generic over
//! the stream, so the host test runs it on an in-memory pipe and on loopback sockets. The bundle
//! binds the port, accepts, and waits for the end (`SetupEnd`).
//!
//! A connection is closed by dropping it: shutdown() is not among what the bundle's SELinux domain
//! may do on a socket.

use crate::google::Google;
use crate::oauth::{self, AuthError, ClientConfig, Flows, Grant, SetupToken, State, Tokens};
use crate::setup::{self, Reject, Request, PORT};
use log::{info, warn};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Semaphore};
use tokio::time::timeout;

/// What the setup page allows.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Connections served at once, at most.
    pub max_connections: usize,
    /// A request is read within this, its answer written within this again.
    pub request_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { max_connections: 8, request_timeout: Duration::from_secs(10) }
    }
}

/// How the setup ends.
#[derive(Debug, PartialEq)]
pub enum SetupEnd {
    /// An account was linked: its grant and the tokens of the exchange.
    Linked(Grant, Tokens),
    /// Google refused the OAuth client at the exchange (`invalid_client` and the like).
    Refused(String),
}

/// What the setup page tells the bundle.
pub struct PageEvents {
    /// How the setup ended (the first end; later ones are answered "linked already").
    pub end: mpsc::Receiver<SetupEnd>,
    /// The QR code's token was used: the setup URL changed.
    pub used: mpsc::UnboundedReceiver<()>,
}

/// The setup page: its token, its flows, its connections.
pub struct SetupPage<G> {
    google: G,
    client: ClientConfig,
    state: Mutex<(SetupToken, Flows)>,
    end: mpsc::Sender<SetupEnd>,
    used: mpsc::UnboundedSender<()>,
    limits: Limits,
    connections: Arc<Semaphore>,
}

impl<G: Google + 'static> SetupPage<G> {
    /// A setup page for `client` at `now`, asking `google`, and what it tells the bundle.
    pub fn start(google: G, client: ClientConfig, limits: Limits, now: Instant) -> (Arc<SetupPage<G>>, PageEvents) {
        let (end, end_rx) = mpsc::channel(1);
        let (used, used_rx) = mpsc::unbounded_channel();
        let page = SetupPage {
            google,
            client,
            state: Mutex::new((SetupToken::new(now), Flows::default())),
            end,
            used,
            limits,
            connections: Arc::new(Semaphore::new(limits.max_connections)),
        };
        (Arc::new(page), PageEvents { end: end_rx, used: used_rx })
    }

    /// The URL of the QR code, with the panel's `address`.
    pub fn url(&self, address: Ipv4Addr) -> String {
        format!("http://{address}:{PORT}/setup?t={}", self.state.lock().expect("setup state").0.value())
    }

    /// A new token when the current one is due; true when it was replaced.
    pub fn rotate_if_due(&self, now: Instant) -> bool {
        self.state.lock().expect("setup state").0.rotate_if_due(now)
    }

    /// A connection accepted: served in a task of its own, or closed at once when
    /// `max_connections` are served already (false).
    pub fn accept(self: &Arc<Self>, stream: TcpStream) -> bool {
        let Ok(permit) = self.connections.clone().try_acquire_owned() else { return false };
        let page = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let local = stream.local_addr().ok();
            page.serve(stream, local).await;
        });
        true
    }

    /// One connection: its request read within the time, answered, closed. `local`: the address
    /// the browser reached the panel on, where Google's answer comes back through the relay page.
    pub async fn serve<S: AsyncRead + AsyncWrite + Unpin>(&self, mut stream: S, local: Option<SocketAddr>) {
        let answer = match timeout(self.limits.request_timeout, read_head(&mut stream)).await {
            // too slow: closed without an answer
            Err(_) => return,
            Ok(Err(why)) => setup::reject(why),
            Ok(Ok(head)) => match setup::parse(&head) {
                Ok(request) => self.answer(request, local).await,
                Err(why) => setup::reject(why),
            },
        };
        let _ = timeout(self.limits.request_timeout, stream.write_all(&answer)).await;
    }

    /// The answer to a request of the setup page.
    async fn answer(&self, request: Request, local: Option<SocketAddr>) -> Vec<u8> {
        const AGAIN: &str = "Scan the code on the panel to try again.";
        match request {
            Request::Setup { token } => {
                let Some(SocketAddr::V4(local)) = local else {
                    return setup::page("500 Internal Server Error", "The panel does not know its address.");
                };
                let url = {
                    let mut state = self.state.lock().expect("setup state");
                    let now = Instant::now();
                    if !state.0.take(&token, now) {
                        return setup::page("403 Forbidden", &format!("This link is old or was used. {AGAIN}"));
                    }
                    let (nonce, pkce) = state.1.begin(now);
                    let flow = State { panel: local.to_string(), nonce };
                    oauth::auth_url(&self.client, &pkce.challenge, &flow.encode())
                };
                let _ = self.used.send(());
                info!("the setup page: a link begins, to Google's consent screen");
                setup::redirect(&url)
            }
            Request::Denied { error, state } => {
                if let Some(flow) = State::decode(&state) {
                    self.state.lock().expect("setup state").1.take(&flow.nonce, Instant::now());
                }
                info!("the setup page: Google answered {error}");
                setup::page("200 OK", &format!("The calendar was not linked ({error}). {AGAIN}"))
            }
            Request::Callback { code, state } => {
                let verifier = State::decode(&state).and_then(|flow| self.state.lock().expect("setup state").1.take(&flow.nonce, Instant::now()));
                let Some(verifier) = verifier else {
                    info!("the setup page: an answer of no open flow");
                    return setup::page("400 Bad Request", &format!("This sign-in is old, was used or is not of this panel. {AGAIN}"));
                };
                match self.google.exchange(&self.client, &code, &verifier).await {
                    Ok(tokens) => {
                        let Some(refresh_token) = tokens.refresh_token.clone() else {
                            warn!("the setup page: Google sent no refresh token");
                            return setup::page("502 Bad Gateway", &format!("Google sent no lasting access. {AGAIN}"));
                        };
                        let grant = Grant { client_id: self.client.client_id.clone(), refresh_token };
                        match self.end.try_send(SetupEnd::Linked(grant, tokens)) {
                            Ok(()) => setup::page("200 OK", "Done. You can close this page."),
                            Err(_) => setup::page("409 Conflict", "The panel was linked by another sign-in just now."),
                        }
                    }
                    Err(AuthError::Scope) => {
                        info!("the setup page: the calendar permission was not granted");
                        setup::page("403 Forbidden", &format!("The calendar permission was not granted. {AGAIN}"))
                    }
                    Err(err @ AuthError::Client(_)) => {
                        warn!("the setup page: {err}");
                        let _ = self.end.try_send(SetupEnd::Refused(err.to_string()));
                        setup::page("502 Bad Gateway", "Google refused the panel's OAuth client. The owner of the panel has to set it up again.")
                    }
                    Err(err) => {
                        warn!("the setup page: the code exchange failed: {err}");
                        setup::page("502 Bad Gateway", &format!("Google did not complete the link. {AGAIN}"))
                    }
                }
            }
        }
    }
}

/// A request's head, up to its blank line, at most `setup::MAX_REQUEST` bytes.
async fn read_head<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>, Reject> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{ApiError, Read};
    use crate::oauth::Pkce;
    use std::collections::HashMap;
    use tokio::io::duplex;
    use tokio::net::TcpListener;

    /// Google whose token endpoint gives one scripted answer and writes down the verifiers.
    struct Exchange {
        answer: Mutex<Option<Result<Tokens, AuthError>>>,
        verifiers: Mutex<Vec<String>>,
    }

    impl Exchange {
        fn new(answer: Result<Tokens, AuthError>) -> Exchange {
            Exchange { answer: Mutex::new(Some(answer)), verifiers: Mutex::default() }
        }
    }

    #[async_trait::async_trait]
    impl Google for Arc<Exchange> {
        async fn exchange(&self, _: &ClientConfig, _: &str, verifier: &str) -> Result<Tokens, AuthError> {
            self.verifiers.lock().unwrap().push(verifier.to_string());
            self.answer.lock().unwrap().take().unwrap_or_else(|| Err(AuthError::Grant("invalid_grant: used".into())))
        }

        async fn refresh(&self, _: &ClientConfig, _: &str) -> Result<Tokens, AuthError> {
            unreachable!("no refresh on the setup page")
        }

        async fn events(&self, _: &str, _: i64) -> Result<Read, ApiError> {
            unreachable!("no events on the setup page")
        }
    }

    fn client() -> ClientConfig {
        ClientConfig::parse(r#"{"web":{"client_id":"id-1","client_secret":"s","redirect_uris":["https://relay.example.org/oauth/"]}}"#).unwrap()
    }

    fn tokens() -> Tokens {
        Tokens { access_token: "ya29.a".into(), expires_in: 3599, refresh_token: Some("1//r".into()) }
    }

    fn page(answer: Result<Tokens, AuthError>, limits: Limits) -> (Arc<SetupPage<Arc<Exchange>>>, PageEvents, Arc<Exchange>) {
        let google = Arc::new(Exchange::new(answer));
        let (page, events) = SetupPage::start(google.clone(), client(), limits, Instant::now());
        (page, events, google)
    }

    const PANEL: &str = "192.168.1.23:8765";

    /// One request through an in-memory pipe: the answer as the phone reads it, up to the close.
    async fn ask<G: Google + 'static>(page: &SetupPage<G>, request: &[u8]) -> String {
        let (mut phone, panel) = duplex(64 * 1024);
        phone.write_all(request).await.unwrap();
        page.serve(panel, Some(PANEL.parse().unwrap())).await;
        let mut answer = String::new();
        phone.read_to_string(&mut answer).await.unwrap();
        answer
    }

    fn get(target: &str) -> Vec<u8> {
        format!("GET {target} HTTP/1.1\r\nHost: {PANEL}\r\n\r\n").into_bytes()
    }

    fn token_of(url: &str) -> String {
        url.split_once("?t=").unwrap().1.to_string()
    }

    fn query(url: &str) -> HashMap<String, String> {
        form_urlencoded::parse(url.split_once('?').unwrap().1.as_bytes()).into_owned().collect()
    }

    #[tokio::test]
    async fn from_the_qr_code_to_the_grant() {
        let (page, mut events, google) = page(Ok(tokens()), Limits::default());
        let url = page.url(Ipv4Addr::new(192, 168, 1, 23));
        assert!(url.starts_with("http://192.168.1.23:8765/setup?t="), "{url}");
        let answer = ask(&page, &get(&format!("/setup?t={}", token_of(&url)))).await;
        assert!(answer.starts_with("HTTP/1.1 302 Found\r\n"), "{answer}");
        // a new token for the QR code
        assert!(events.used.try_recv().is_ok());
        assert_ne!(page.url(Ipv4Addr::new(192, 168, 1, 23)), url);
        // to Google with the address the phone reached, PKCE and the nonce
        let location = answer.lines().find_map(|l| l.strip_prefix("Location: ")).unwrap();
        let q = query(location);
        let flow = State::decode(&q["state"]).unwrap();
        assert_eq!(flow.panel, PANEL);
        // the relay page sends the phone back with the code
        let callback = format!("/callback?code=4%2F0Ab&state={}", q["state"]);
        let answer = ask(&page, &get(&callback)).await;
        assert!(answer.starts_with("HTTP/1.1 200 OK") && answer.contains("Done."), "{answer}");
        // the exchange had the verifier of the challenge that went to Google
        let verifier = google.verifiers.lock().unwrap()[0].clone();
        assert_eq!(Pkce::of(verifier).challenge, q["code_challenge"]);
        match events.end.try_recv().unwrap() {
            SetupEnd::Linked(grant, t) => assert_eq!((grant.client_id.as_str(), grant.refresh_token.as_str(), t), ("id-1", "1//r", tokens())),
            other => panic!("{other:?}"),
        }
        // the flow is over: the same answer again is refused
        assert!(ask(&page, &get(&callback)).await.starts_with("HTTP/1.1 400 "));
    }

    #[tokio::test]
    async fn a_token_is_good_once() {
        let (page, _events, _) = page(Ok(tokens()), Limits::default());
        let token = token_of(&page.url(Ipv4Addr::LOCALHOST));
        assert!(ask(&page, &get("/setup?t=wrong")).await.starts_with("HTTP/1.1 403 "));
        assert!(ask(&page, &get(&format!("/setup?t={token}"))).await.starts_with("HTTP/1.1 302 "));
        assert!(ask(&page, &get(&format!("/setup?t={token}"))).await.starts_with("HTTP/1.1 403 "));
    }

    #[tokio::test]
    async fn a_refused_client_ends_the_setup() {
        let (page, mut events, _) = page(Err(AuthError::Client("invalid_client: Unauthorized".into())), Limits::default());
        let answer = ask(&page, &get(&format!("/setup?t={}", token_of(&page.url(Ipv4Addr::LOCALHOST))))).await;
        let state = query(answer.lines().find_map(|l| l.strip_prefix("Location: ")).unwrap())["state"].clone();
        let answer = ask(&page, &get(&format!("/callback?code=c&state={state}"))).await;
        assert!(answer.starts_with("HTTP/1.1 502 ") && answer.contains("refused the panel"), "{answer}");
        assert!(matches!(events.end.try_recv().unwrap(), SetupEnd::Refused(why) if why.contains("invalid_client")));
    }

    #[tokio::test]
    async fn the_head_is_bounded() {
        let (page, _events, _) = page(Ok(tokens()), Limits::default());
        // 5000 bytes and no end of the head
        let answer = ask(&page, &[b'a'; 5000]).await;
        assert!(answer.starts_with("HTTP/1.1 431 "), "{answer}");
        // a whole head, too long
        let mut long = get("/setup?t=x");
        let line = long.windows(2).position(|w| w == b"\r\n").unwrap() + 2;
        long.splice(line..line, format!("X-Pad: {}\r\n", "a".repeat(setup::MAX_REQUEST)).into_bytes());
        assert!(ask(&page, &long).await.starts_with("HTTP/1.1 431 "));
    }

    #[tokio::test]
    async fn a_slow_request_is_closed_without_an_answer() {
        let limits = Limits { request_timeout: Duration::from_millis(100), ..Limits::default() };
        let (page, _events, _) = page(Ok(tokens()), limits);
        let (mut phone, panel) = duplex(4096);
        // the head never ends; the phone keeps its end open
        phone.write_all(b"GET /setup?t=x HTTP/1.1\r\nHost: x\r\n").await.unwrap();
        let started = Instant::now();
        page.serve(panel, None).await;
        assert!(started.elapsed() >= Duration::from_millis(100) && started.elapsed() < Duration::from_secs(5));
        let mut answer = Vec::new();
        phone.read_to_end(&mut answer).await.unwrap();
        assert!(answer.is_empty(), "no answer, only the close");
    }

    #[tokio::test]
    async fn connections_over_the_limit_are_closed_at_once() {
        let limits = Limits { max_connections: 1, request_timeout: Duration::from_secs(5) };
        let (page, _events, _) = page(Ok(tokens()), limits);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = page.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                server.accept(stream);
            }
        });
        // the first connection holds the only slot
        let first = TcpStream::connect(address).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        // the second is closed at once, long before the request timeout
        let mut second = TcpStream::connect(address).await.unwrap();
        let mut byte = [0u8; 1];
        let closed = timeout(Duration::from_secs(2), second.read(&mut byte)).await.expect("closed at once");
        assert!(matches!(closed, Ok(0) | Err(_)), "{closed:?}");
        // the first goes away: its slot is free again
        drop(first);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut third = TcpStream::connect(address).await.unwrap();
        third.write_all(&get("/nothing")).await.unwrap();
        let mut answer = String::new();
        timeout(Duration::from_secs(2), third.read_to_string(&mut answer)).await.unwrap().unwrap();
        assert!(answer.starts_with("HTTP/1.1 404 "), "{answer}");
    }
}
