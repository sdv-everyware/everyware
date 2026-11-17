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

//! The link's decisions, apart from the bundle's loop and its sleeps: what one read of the agenda
//! gives (`Session`: the access token renewed when it is due and once more after a 401, a new
//! refresh token and the granted scope kept), what a change of the screen gives
//! (`Session::change`, change.rs) and what each outcome leaves in the bundle's directory and
//! publishes (`Link`). The bundle (../../src/lib.rs) loops, sleeps and publishes what these return.
//!
//! The outcomes and the screen:
//!
//! | outcome | the bundle's directory | the agenda |
//! |---|---|---|
//! | a read | `agenda.pb` | `CALENDAR_CONNECTED` with the events |
//! | `invalid_grant`, the scope gone | `grant.json` removed | `CALENDAR_EXPIRED`, no events, the setup page again |
//! | the client refused, the Calendar API off | `grant.json` kept (a reset secret keeps the client's id) | `CALENDAR_NO_CLIENT`, no events, until `oauth-client.json` changes |
//! | anything else | | the last agenda stays, with `read_failed` (`failed`) once the reads have failed for `FAILED_AFTER`; its `updated` tells its age |
//!
//! When the next read is (`Pace`): a minute after a read; after a failure 2 s, then twice as long
//! each time up to 5 minutes, so an outage of Google or of the network is not asked every minute.

use crate::change::{self, Change, Failure, Refusal};
use crate::events::{ApiError, Read};
use crate::google::Google;
use crate::oauth::{Access, AuthError, ClientConfig, Grant, Tokens, READ_SCOPE};
use everyware_bundle_store::Store;
use everyware_catalog::calendar::{Agenda, CalendarLink};
use everyware_catalog::calendar_service::CalendarFailure;
use protobuf::Message;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The owner's OAuth client (Google's client JSON): scripts/calendar-client.sh puts it there.
pub const CLIENT_FILE: &str = "oauth-client.json";
const MAX_CLIENT_FILE: usize = 16 * 1024;
/// The linked account's refresh token (0600).
pub const GRANT_FILE: &str = "grant.json";
const MAX_GRANT_FILE: usize = 4 * 1024;
/// The last agenda (0600: titles, places, the account's address), published again at the next
/// start until the first new read.
pub const LAST_AGENDA: &str = "agenda.pb";
/// Older than this the last agenda's events are not shown at a start.
pub const LAST_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
/// An access token is renewed when it has less than this left.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(120);
/// A read every this long while the reads succeed: a new event is on the screen within a minute.
/// A read is the 7-day window with the `fields` filter, a few kB; 1440 a day. Google's
/// incremental sync (`syncToken`) does not fit: it cannot be combined with timeMin, timeMax and
/// orderBy, so it would sync the whole calendar.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);
/// After a failed read the next one waits this, then twice as long each time up to `RETRY_MAX`.
pub const RETRY_FIRST: Duration = Duration::from_secs(2);
/// The longest wait after failures (an outage of Google: 429, 5xx, or of the network).
pub const RETRY_MAX: Duration = Duration::from_secs(300);
/// The screen hears of failed reads (`read_failed`) once they have failed for this long, the
/// waits between them counted (the fifth failure in a row): at a start the bundle runs before
/// the network is up, and its first reads fail for a few seconds.
pub const FAILED_AFTER: Duration = Duration::from_secs(30);
/// An agenda that did not change (a read every minute changes only `updated`) is written again
/// at most this often, to spare the flash; a changed one at once.
pub const KEEP_EVERY: Duration = Duration::from_secs(3600);
/// The setup URL for the development flow, in the bundle's directory (0600): the bundle's debug
/// lines do not reach logcat, and the URL holds a one-time token. Gone once the setup ends.
pub const SETUP_URL_FILE: &str = "setup-url";

/// When the next read is: `POLL_INTERVAL` after a read; after failures `RETRY_FIRST`, then twice
/// as long each time up to `RETRY_MAX`. And whether the reads fail for long enough to say so
/// (`failing`).
#[derive(Debug, Default)]
pub struct Pace {
    /// The last wait after a failure, while the reads fail.
    retry: Option<Duration>,
    /// The waits between the failures in a row so far.
    waited: Duration,
}

impl Pace {
    /// The wait after a read that succeeded; the failures start again from `RETRY_FIRST`.
    pub fn after_read(&mut self) -> Duration {
        (self.retry, self.waited) = (None, Duration::ZERO);
        POLL_INTERVAL
    }

    /// The wait after a read that failed.
    pub fn after_failure(&mut self) -> Duration {
        self.waited += self.retry.unwrap_or_default();
        let wait = self.retry.map_or(RETRY_FIRST, |last| (last * 2).min(RETRY_MAX));
        self.retry = Some(wait);
        wait
    }

    /// Whether the reads have failed for `FAILED_AFTER` (since the last read that worked, or the
    /// start): the screen is told then, not at the first failure of a start.
    pub fn failing(&self) -> bool {
        self.waited >= FAILED_AFTER
    }
}

/// What a read of the agenda gave.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The calendar's events.
    Read(Read),
    /// The grant is not good any more (`invalid_grant`, the calendar scope gone): link the account
    /// again.
    Revoked(String),
    /// Google refused the OAuth client or its project (`invalid_client`, the Calendar API turned
    /// off): it stays so until a new client file.
    Refused(String),
    /// Anything else (the network, a quota, Google): try again later.
    Again(String),
}

/// An agenda with `link` and nothing else.
pub fn agenda_of(link: CalendarLink) -> Agenda {
    let mut agenda = Agenda::new();
    agenda.link = link.into();
    agenda
}

/// Connected with `grant`, not read yet: `updated` 0, no events, the access granted.
pub fn not_read(grant: &Grant) -> Agenda {
    let mut agenda = agenda_of(CalendarLink::CALENDAR_CONNECTED);
    agenda.can_change = grant.access() == Access::Change;
    agenda
}

/// The link's state of an agenda.
pub fn link_of(agenda: &Agenda) -> CalendarLink {
    agenda.link.enum_value_or_default()
}

/// A read failed: the agenda published last as it was (its events, its `updated`), with
/// `read_failed`, so the screen tells a failed read from one not done yet. None when the last
/// one is not connected. The next read clears it (`Link::connected` makes a new agenda).
pub fn failed(last: Option<&Agenda>) -> Option<Agenda> {
    let mut agenda = last.filter(|a| link_of(a) == CalendarLink::CALENDAR_CONNECTED)?.clone();
    agenda.read_failed = true;
    Some(agenda)
}

/// The reads of a linked account: the grant and the access token.
pub struct Session {
    client: ClientConfig,
    grant: Grant,
    /// The access token and until when it is good.
    access: Option<(String, Instant)>,
}

impl Session {
    /// The reads of `grant` with `client`; `tokens`: what a code exchange gave just now.
    pub fn new(client: ClientConfig, grant: Grant, tokens: Option<Tokens>, now: Instant) -> Session {
        let access = tokens.map(|t| (t.access_token, now + Duration::from_secs(t.expires_in)));
        Session { client, grant, access }
    }

    /// The grant as it is now (a refresh may have brought a new refresh token or scope).
    pub fn grant(&self) -> &Grant {
        &self.grant
    }

    /// Whether Google granted the change of the events (the grant's scope, as the last token
    /// answer or change said).
    pub fn can_change(&self) -> bool {
        self.grant.access() == Access::Change
    }

    /// An access token with `REFRESH_MARGIN` left at `now`: the one there, or a new one (a new
    /// refresh token and a new scope go into `store` at once).
    async fn token<G: Google + ?Sized>(&mut self, google: &G, store: &Store, now: Instant) -> Result<String, AuthError> {
        if self.access.as_ref().is_none_or(|(_, until)| now + REFRESH_MARGIN >= *until) {
            let tokens = google.refresh(&self.client, &self.grant.refresh_token).await?;
            tokens.calendar()?;
            self.renewed(tokens, store, now);
        }
        Ok(self.access.as_ref().map(|(token, _)| token.clone()).unwrap_or_default())
    }

    /// One read at `now` (`unix`: the same as unix time): the access token renewed when it has
    /// less than `REFRESH_MARGIN` left, the events, and once more with a new access token when
    /// the API refused the one it had. A new refresh token goes into `store` at once.
    pub async fn read<G: Google + ?Sized>(&mut self, google: &G, store: &Store, now: Instant, unix: i64) -> Outcome {
        for _ in 0..2 {
            let token = match self.token(google, store, now).await {
                Ok(token) => token,
                Err(err @ (AuthError::Grant(_) | AuthError::Scope)) => return Outcome::Revoked(err.to_string()),
                Err(err @ AuthError::Client(_)) => return Outcome::Refused(err.to_string()),
                Err(err @ AuthError::Other(_)) => return Outcome::Again(err.to_string()),
            };
            match google.events(&token, unix).await {
                Ok(read) => return Outcome::Read(read),
                // renewed and tried once more
                Err(ApiError::Unauthorized) => self.access = None,
                Err(err @ ApiError::Disabled(_)) => return Outcome::Refused(err.to_string()),
                Err(err @ ApiError::Scope(_)) => return Outcome::Revoked(err.to_string()),
                Err(err @ ApiError::Other(_)) => return Outcome::Again(err.to_string()),
            }
        }
        Outcome::Again("the access token was refused twice".into())
    }

    /// One change of the screen at `now` (change.rs): the event read as Google has it, the rule
    /// checked on it, the change made on that version. Once more with a new access token when the
    /// API refused the one it had, and once more from the read when the event changed in between
    /// (412). Google saying that the grant lacks the scope makes the grant read only (kept in
    /// `store`), so the next agenda offers no change.
    pub async fn change<G: Google + ?Sized>(&mut self, google: &G, store: &Store, change: &Change, now: Instant) -> Result<(), Failure> {
        let (mut renewed, mut read_again) = (false, false);
        loop {
            let token = self.token(google, store, now).await.map_err(|err| match err {
                AuthError::Grant(_) | AuthError::Scope => Failure::new(CalendarFailure::CALENDAR_FAILURE_NOT_LINKED, err.to_string()),
                AuthError::Client(_) => Failure::new(CalendarFailure::CALENDAR_FAILURE_REFUSED, err.to_string()),
                AuthError::Other(_) => Failure::again(err.to_string()),
            })?;
            match apply(google, &token, change).await {
                Ok(()) => return Ok(()),
                Err(Refusal::Unauthorized) if !renewed => {
                    renewed = true;
                    self.access = None;
                }
                Err(Refusal::Changed) if !read_again => read_again = true,
                Err(Refusal::Unauthorized) => return Err(Failure::again("the access token was refused twice")),
                Err(Refusal::Changed) => return Err(Failure::again("the event changed twice while it was being changed")),
                Err(Refusal::Failed(failure)) => {
                    if failure.kind == CalendarFailure::CALENDAR_FAILURE_READ_ONLY {
                        self.read_only(store);
                    }
                    return Err(failure);
                }
            }
        }
    }

    /// The account unlinked (CalendarService.Disconnect): the grant ended at Google. Ok also when
    /// Google says it was ended before; `AGAIN` when Google could not be told, and then the
    /// account stays linked (the caller forgets the grant only after Ok, `Link::disconnected`).
    pub async fn disconnect<G: Google + ?Sized>(&mut self, google: &G) -> Result<(), Failure> {
        match google.revoke(&self.grant.refresh_token).await {
            Err(AuthError::Other(why)) => Err(Failure::again(format!("Google could not be told: {why}"))),
            _ => {
                self.access = None;
                Ok(())
            }
        }
    }

    /// The tokens of a refresh: a new refresh token and a new scope kept in place of the old.
    fn renewed(&mut self, tokens: Tokens, store: &Store, now: Instant) {
        let mut changed = false;
        if let Some(refresh_token) = tokens.refresh_token.filter(|t| *t != self.grant.refresh_token) {
            self.grant.refresh_token = refresh_token;
            changed = true;
        }
        if let Some(scope) = tokens.scope.filter(|s| *s != self.grant.scope) {
            log::info!("Google grants {:?} access", Access::granted(&scope).unwrap_or(Access::Read));
            self.grant.scope = scope;
            changed = true;
        }
        if changed {
            self.keep(store);
        }
        log::debug!("a new access token for {} s", tokens.expires_in);
        self.access = Some((tokens.access_token, now + Duration::from_secs(tokens.expires_in)));
    }

    /// Google said the grant lacks the change of the events: read access from now on.
    fn read_only(&mut self, store: &Store) {
        if self.grant.access() != Access::Read {
            log::warn!("Google says the grant may only read the calendar");
            self.grant.scope = READ_SCOPE.to_string();
            self.keep(store);
        }
    }

    fn keep(&self, store: &Store) {
        if let Err(err) = store.save_private(GRANT_FILE, self.grant.to_json().as_bytes()) {
            log::error!("the grant not kept: {err}");
        }
    }
}

/// One try of `change` with the access token `token`: the event read, the rule checked on it, the
/// change made with the event's ETag.
async fn apply<G: Google + ?Sized>(google: &G, token: &str, change: &Change) -> Result<(), Refusal> {
    let network = |why: String| Refusal::Failed(Failure::again(why));
    let reply = google.call(token, &change.get()).await.map_err(network)?;
    if reply.status != 200 {
        return Err(change::refusal(reply.status, &reply.body));
    }
    let item: Value = serde_json::from_slice(&reply.body).map_err(|_| Refusal::Failed(Failure::again("events.get: not JSON")))?;
    let Some(call) = change.call(&item).map_err(Refusal::Failed)? else {
        return Ok(());
    };
    let reply = google.call(token, &call).await.map_err(network)?;
    if (200..300).contains(&reply.status) {
        Ok(())
    } else {
        Err(change::refusal(reply.status, &reply.body))
    }
}

/// The link's files in the bundle's directory, and the client Google refused.
pub struct Link {
    store: Arc<Store>,
    /// The client file as `client` read it last.
    file: Option<Vec<u8>>,
    /// The client file Google refused: no client while the file is the same.
    refused: Option<Vec<u8>>,
    /// The connected agenda kept last, without its `updated`, and when.
    kept: Option<(Agenda, Instant)>,
}

impl Link {
    /// The link's files in `store`.
    pub fn new(store: Arc<Store>) -> Link {
        Link { store, file: None, refused: None, kept: None }
    }

    /// The bundle's directory.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// What the last start left: whether the link had expired (however long ago: no account was
    /// linked since), and its agenda when it was connected and the agenda is a day old at most.
    pub fn last(&self) -> (bool, Option<Agenda>) {
        let expired = self.store.load::<Agenda>(LAST_AGENDA, Duration::MAX).is_some_and(|a| link_of(&a) == CalendarLink::CALENDAR_EXPIRED);
        let kept = self.store.load::<Agenda>(LAST_AGENDA, LAST_MAX_AGE).filter(|a| link_of(a) == CalendarLink::CALENDAR_CONNECTED);
        (expired, kept)
    }

    /// The owner's client, or why there is none: no file, a file that is not a usable client, or
    /// the file whose client Google refused (until the file changes).
    pub fn client(&mut self) -> Result<ClientConfig, String> {
        let bytes = self.store.read(CLIENT_FILE, MAX_CLIENT_FILE).ok_or_else(|| format!("no {CLIENT_FILE} (or over {MAX_CLIENT_FILE} bytes)"))?;
        if self.refused.as_ref() == Some(&bytes) {
            return Err(format!("Google refused the client in {CLIENT_FILE}: a new one is needed"));
        }
        let client = ClientConfig::parse(&String::from_utf8_lossy(&bytes)).map_err(|why| format!("{CLIENT_FILE}: {why}"))?;
        self.file = Some(bytes);
        Ok(client)
    }

    /// The kept grant, when it is one of `client`'s.
    pub fn grant(&self, client: &ClientConfig) -> Option<Grant> {
        let grant = Grant::parse(&self.store.read(GRANT_FILE, MAX_GRANT_FILE)?)?;
        if grant.client_id != client.client_id {
            log::warn!("the kept grant is of another OAuth client: the account has to be linked again");
            return None;
        }
        Some(grant)
    }

    /// An account was linked just now: its grant kept; connected, not read yet (`updated` 0).
    pub fn linked(&self, grant: &Grant) -> Agenda {
        if let Err(err) = self.store.save_private(GRANT_FILE, grant.to_json().as_bytes()) {
            log::error!("the grant not kept, the account has to be linked again after a restart: {err}");
        }
        not_read(grant)
    }

    /// A read at `unix` (`now` on the monotonic clock) with the change of the events granted or
    /// not (`can_change`): the connected agenda, kept when it changed or when the last one was
    /// kept `KEEP_EVERY` ago. Without the change no event offers one.
    pub fn connected(&mut self, read: Read, can_change: bool, unix: i64, now: Instant) -> Agenda {
        let mut agenda = agenda_of(CalendarLink::CALENDAR_CONNECTED);
        agenda.calendar = read.calendar;
        agenda.events = read.events;
        agenda.can_change = can_change;
        if !can_change {
            for event in &mut agenda.events {
                (event.can_respond, event.can_delete) = (false, false);
            }
        }
        let content = agenda.clone();
        agenda.updated = unix;
        let same = self.kept.as_ref().is_some_and(|(kept, at)| *kept == content && now.saturating_duration_since(*at) < KEEP_EVERY);
        if !same {
            self.keep(&agenda);
            self.kept = Some((content, now));
        }
        agenda
    }

    /// The setup URL as it is now into `SETUP_URL_FILE` (0600); empty: the file removed.
    pub fn setup_url(&self, url: &str) {
        let written = if url.is_empty() { self.store.remove(SETUP_URL_FILE) } else { self.store.save_private(SETUP_URL_FILE, format!("{url}\n").as_bytes()) };
        if let Err(err) = written {
            log::warn!("{SETUP_URL_FILE}: {err}");
        }
    }

    /// The account unlinked from the screen (its grant ended at Google): the grant forgotten, and
    /// the events with it; not connected, kept (a restart shows the setup page, not old events).
    pub fn disconnected(&mut self) -> Agenda {
        self.forget_grant();
        let agenda = agenda_of(CalendarLink::CALENDAR_NOT_CONNECTED);
        self.keep(&agenda);
        self.kept = None;
        agenda
    }

    /// A kept grant of another OAuth client than `client` (the owner put a new client on the
    /// panel): useless with the new client, still good at Google until it is revoked.
    pub fn grant_of_another_client(&self, client: &ClientConfig) -> Option<Grant> {
        Grant::parse(&self.store.read(GRANT_FILE, MAX_GRANT_FILE)?).filter(|grant| grant.client_id != client.client_id)
    }

    /// `grant.json` removed.
    pub fn forget_grant(&self) {
        if let Err(err) = self.store.remove(GRANT_FILE) {
            log::error!("the grant not removed: {err}");
        }
    }

    /// The grant is not good any more: forgotten, and the events with it; expired, kept (it
    /// stays expired across restarts until an account is linked).
    pub fn revoked(&mut self) -> Agenda {
        self.forget_grant();
        let agenda = agenda_of(CalendarLink::CALENDAR_EXPIRED);
        self.keep(&agenda);
        self.kept = None;
        agenda
    }

    /// Google refused the client (or its project's Calendar API is off): no client until the file
    /// changes, and the events dropped (the screen must not show them as current). The grant
    /// stays: a client whose secret was reset keeps its id and its grants.
    pub fn refused(&mut self) -> Agenda {
        self.refused = self.file.clone();
        let agenda = agenda_of(CalendarLink::CALENDAR_NO_CLIENT);
        self.keep(&agenda);
        self.kept = None;
        agenda
    }

    /// `agenda` kept for the next start, readable by the bundle only.
    fn keep(&self, agenda: &Agenda) {
        let kept = agenda.write_to_bytes().map_err(std::io::Error::other).and_then(|bytes| self.store.save_private(LAST_AGENDA, &bytes));
        if let Err(err) = kept {
            log::warn!("the agenda not kept: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::ApiCall;
    use crate::oauth::CHANGE_SCOPE;
    use everyware_catalog::calendar::{AgendaEvent, Rsvp};
    use everyware_https::Reply;
    use std::collections::VecDeque;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::SystemTime;

    const CLIENT: &str = r#"{"web":{"client_id":"id-1","client_secret":"s","redirect_uris":["https://relay.example.org/oauth/"]}}"#;

    /// Google from a script: each call takes the next answer; the calls are written down.
    #[derive(Default)]
    struct Script {
        refresh: Mutex<VecDeque<Result<Tokens, AuthError>>>,
        events: Mutex<VecDeque<Result<Read, ApiError>>>,
        api: Mutex<VecDeque<Result<Reply, String>>>,
        revoke: Mutex<VecDeque<Result<(), AuthError>>>,
        calls: Mutex<Vec<String>>,
        bodies: Mutex<Vec<String>>,
    }

    impl Script {
        fn new(refresh: Vec<Result<Tokens, AuthError>>, events: Vec<Result<Read, ApiError>>) -> Script {
            Script { refresh: Mutex::new(refresh.into()), events: Mutex::new(events.into()), ..Script::default() }
        }

        /// The answers of the changes' calls (status, body; an empty body for none), and the refreshes.
        fn api(refresh: Vec<Result<Tokens, AuthError>>, api: Vec<Result<(u16, &str), &str>>) -> Script {
            let api = api.into_iter().map(|a| a.map(|(status, body)| Reply { status, content_type: "application/json".into(), body: body.as_bytes().to_vec() }).map_err(str::to_string));
            Script { refresh: Mutex::new(refresh.into()), api: Mutex::new(api.collect()), ..Script::default() }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Google for Script {
        async fn exchange(&self, _: &ClientConfig, _: &str, _: &str) -> Result<Tokens, AuthError> {
            unreachable!("no exchange in a session")
        }

        async fn refresh(&self, _: &ClientConfig, refresh_token: &str) -> Result<Tokens, AuthError> {
            self.calls.lock().unwrap().push(format!("refresh {refresh_token}"));
            self.refresh.lock().unwrap().pop_front().expect("a scripted refresh")
        }

        async fn revoke(&self, token: &str) -> Result<(), AuthError> {
            self.calls.lock().unwrap().push(format!("revoke {token}"));
            self.revoke.lock().unwrap().pop_front().expect("a scripted revocation")
        }

        async fn events(&self, access_token: &str, _: i64) -> Result<Read, ApiError> {
            self.calls.lock().unwrap().push(format!("events {access_token}"));
            self.events.lock().unwrap().pop_front().expect("scripted events")
        }

        // "PATCH abc a1 \"7\"": the method, the event, the token, the condition
        async fn call(&self, access_token: &str, call: &ApiCall) -> Result<Reply, String> {
            let event = call.url.split('?').next().unwrap().rsplit('/').next().unwrap();
            let condition = call.if_match.as_deref().map(|e| format!(" {e}")).unwrap_or_default();
            self.calls.lock().unwrap().push(format!("{} {event} {access_token}{condition}", call.method));
            if let Some(body) = &call.body {
                self.bodies.lock().unwrap().push(body.clone());
            }
            self.api.lock().unwrap().pop_front().expect("a scripted answer")
        }
    }

    fn dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("everyware_calendar_link_{test}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(CLIENT_FILE), CLIENT).unwrap();
        dir
    }

    fn client() -> ClientConfig {
        ClientConfig::parse(CLIENT).unwrap()
    }

    fn grant() -> Grant {
        Grant { client_id: "id-1".into(), refresh_token: "1//r".into(), scope: READ_SCOPE.into() }
    }

    fn change_grant() -> Grant {
        Grant { scope: format!("{READ_SCOPE} {CHANGE_SCOPE}"), ..grant() }
    }

    fn tokens(access: &str, refresh: Option<&str>) -> Tokens {
        Tokens { access_token: access.into(), expires_in: 3600, refresh_token: refresh.map(str::to_string), scope: None }
    }

    fn read(n: usize) -> Read {
        let events = (0..n)
            .map(|i| {
                let mut e = AgendaEvent::new();
                e.id = format!("e{i}");
                e
            })
            .collect();
        Read { calendar: "anna@example.org".into(), events }
    }

    fn mode(path: PathBuf) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn a_401_gives_one_refresh_and_one_more_read() {
        let d = dir("401");
        let store = Store::at(&d);
        let now = Instant::now();
        let google = Script::new(vec![Ok(tokens("a2", None))], vec![Err(ApiError::Unauthorized), Ok(read(2))]);
        let mut session = Session::new(client(), grant(), Some(tokens("a1", Some("1//r"))), now);
        assert_eq!(session.read(&google, &store, now, 0).await, Outcome::Read(read(2)));
        assert_eq!(google.calls(), ["events a1", "refresh 1//r", "events a2"]);
        // refused again after the refresh: not a third time
        let google = Script::new(vec![Ok(tokens("a3", None))], vec![Err(ApiError::Unauthorized), Err(ApiError::Unauthorized)]);
        assert!(matches!(session.read(&google, &store, now, 0).await, Outcome::Again(_)));
        assert_eq!(google.calls(), ["events a2", "refresh 1//r", "events a3"]);
    }

    #[tokio::test]
    async fn the_access_token_is_renewed_before_it_ends() {
        let d = dir("renew");
        let store = Store::at(&d);
        let now = Instant::now();
        let mut session = Session::new(client(), grant(), Some(tokens("a1", Some("1//r"))), now);
        let google = Script::new(vec![], vec![Ok(read(0))]);
        session.read(&google, &store, now + Duration::from_secs(3600) - REFRESH_MARGIN - Duration::from_secs(1), 0).await;
        assert_eq!(google.calls(), ["events a1"]);
        let google = Script::new(vec![Ok(tokens("a2", None))], vec![Ok(read(0))]);
        session.read(&google, &store, now + Duration::from_secs(3600) - REFRESH_MARGIN, 0).await;
        assert_eq!(google.calls(), ["refresh 1//r", "events a2"]);
    }

    #[tokio::test]
    async fn a_new_refresh_token_is_kept_at_once() {
        let d = dir("rotate");
        let store = Store::at(&d);
        let google = Script::new(vec![Ok(tokens("a1", Some("1//new")))], vec![Ok(read(1))]);
        let mut session = Session::new(client(), grant(), None, Instant::now());
        assert_eq!(session.read(&google, &store, Instant::now(), 0).await, Outcome::Read(read(1)));
        assert_eq!(session.grant().refresh_token, "1//new");
        let kept = Grant::parse(&fs::read(d.join(GRANT_FILE)).unwrap()).unwrap();
        assert_eq!((kept.client_id.as_str(), kept.refresh_token.as_str()), ("id-1", "1//new"));
        assert_eq!(mode(d.join(GRANT_FILE)), 0o600);
    }

    #[tokio::test]
    async fn invalid_grant_ends_in_expired() {
        let d = dir("revoked");
        let store = Arc::new(Store::at(&d));
        let mut link = Link::new(store.clone());
        link.linked(&grant());
        link.connected(read(3), false, 1_763_285_400, Instant::now());
        let google = Script::new(vec![Err(AuthError::Grant("invalid_grant: Token has been expired or revoked.".into()))], vec![]);
        let mut session = Session::new(client(), grant(), None, Instant::now());
        assert!(matches!(session.read(&google, &store, Instant::now(), 0).await, Outcome::Revoked(why) if why.contains("invalid_grant")));
        let agenda = link.revoked();
        assert_eq!((link_of(&agenda), agenda.events.len(), agenda.updated), (CalendarLink::CALENDAR_EXPIRED, 0, 0));
        assert!(!d.join(GRANT_FILE).exists(), "the grant is forgotten");
        assert_eq!(mode(d.join(LAST_AGENDA)), 0o600);
        // expired after a restart too, and no events from before
        assert_eq!(Link::new(store).last(), (true, None));
        // the scope gone is the same
        let google = Script::new(vec![Err(AuthError::Scope)], vec![]);
        assert!(matches!(Session::new(client(), grant(), None, Instant::now()).read(&google, &Store::at(&d), Instant::now(), 0).await, Outcome::Revoked(_)));
        let google = Script::new(vec![], vec![Err(ApiError::Scope("insufficientPermissions".into()))]);
        let fresh = Some(tokens("a1", None));
        assert!(matches!(Session::new(client(), grant(), fresh, Instant::now()).read(&google, &Store::at(&d), Instant::now(), 0).await, Outcome::Revoked(_)));
    }

    #[tokio::test]
    async fn a_refused_client_is_final_until_its_file_changes() {
        let d = dir("refused");
        let store = Arc::new(Store::at(&d));
        let mut link = Link::new(store.clone());
        let client = link.client().unwrap();
        link.linked(&grant());
        link.connected(read(3), false, 1_763_285_400, Instant::now());
        // the secret reset in the Cloud console
        let google = Script::new(vec![Err(AuthError::Client("invalid_client: Unauthorized".into()))], vec![]);
        let outcome = Session::new(client.clone(), grant(), None, Instant::now()).read(&google, &store, Instant::now(), 0).await;
        assert!(matches!(&outcome, Outcome::Refused(why) if why.contains("invalid_client")), "{outcome:?}");
        let agenda = link.refused();
        // the events of before are dropped: the screen shows no client, not an old week
        assert_eq!((link_of(&agenda), agenda.events.len(), agenda.calendar.as_str()), (CalendarLink::CALENDAR_NO_CLIENT, 0, ""));
        assert_eq!(Link::new(store.clone()).last(), (false, None));
        assert!(link.client().unwrap_err().contains("refused"));
        // the grant stays: a new secret keeps the client's id
        assert!(link.grant(&client).is_some());
        // the owner puts the client with the new secret: taken
        fs::write(d.join(CLIENT_FILE), CLIENT.replace("\"s\"", "\"s2\"")).unwrap();
        assert_eq!(link.client().unwrap().client_secret, "s2");
        // the Calendar API turned off in the project: refused the same way
        let google = Script::new(vec![], vec![Err(ApiError::Disabled("accessNotConfigured".into()))]);
        let fresh = Some(tokens("a1", None));
        assert!(matches!(Session::new(client, grant(), fresh, Instant::now()).read(&google, &store, Instant::now(), 0).await, Outcome::Refused(_)));
    }

    #[tokio::test]
    async fn the_network_is_tried_again() {
        let d = dir("again");
        let store = Store::at(&d);
        let google = Script::new(vec![Err(AuthError::Other("dns error".into()))], vec![]);
        assert!(matches!(Session::new(client(), grant(), None, Instant::now()).read(&google, &store, Instant::now(), 0).await, Outcome::Again(_)));
        let google = Script::new(vec![], vec![Err(ApiError::Other("HTTP 503".into()))]);
        let fresh = Some(tokens("a1", None));
        assert!(matches!(Session::new(client(), grant(), fresh, Instant::now()).read(&google, &store, Instant::now(), 0).await, Outcome::Again(_)));
    }

    #[test]
    fn the_last_agenda_at_a_start() {
        let d = dir("last");
        let store = Arc::new(Store::at(&d));
        let mut link = Link::new(store.clone());
        assert_eq!(link.last(), (false, None));
        let agenda = link.connected(read(2), false, 1_763_285_400, Instant::now());
        assert_eq!(mode(d.join(LAST_AGENDA)), 0o600);
        assert_eq!(link.last(), (false, Some(agenda)));
        // older than a day: not shown
        let old = SystemTime::now() - LAST_MAX_AGE - Duration::from_secs(60);
        fs::File::options().write(true).open(d.join(LAST_AGENDA)).unwrap().set_modified(old).unwrap();
        assert_eq!(link.last(), (false, None));
        // expired a month ago: still expired
        link.revoked();
        let month = SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
        fs::File::options().write(true).open(d.join(LAST_AGENDA)).unwrap().set_modified(month).unwrap();
        assert_eq!(link.last(), (true, None));
    }

    #[test]
    fn a_read_a_minute_and_a_backoff_on_failures() {
        let mut pace = Pace::default();
        assert_eq!(pace.after_read(), POLL_INTERVAL);
        assert_eq!(POLL_INTERVAL, Duration::from_secs(60));
        // an outage: 2, 4, 8 ... s, never more often, at most 5 minutes apart
        let waits: Vec<u64> = (0..10).map(|_| pace.after_failure().as_secs()).collect();
        assert_eq!(waits, [2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
        // an hour of outage is a handful of requests, not one a minute or a second
        let mut pace = Pace::default();
        let (mut spent, mut requests) = (Duration::ZERO, 0);
        while spent < Duration::from_secs(3600) {
            spent += pace.after_failure();
            requests += 1;
        }
        assert!(requests <= 20, "{requests}");
        // a read that works again: the minute again, and the next failure from 2 s
        assert_eq!(pace.after_read(), POLL_INTERVAL);
        assert_eq!(pace.after_failure(), RETRY_FIRST);
    }

    #[test]
    fn failed_reads_are_said_after_half_a_minute() {
        // a start before the network: the first four failures (0, 2, 6 and 14 s in) are not
        // said, the fifth (30 s in) is
        let mut pace = Pace::default();
        let failing: Vec<bool> = (0..6).map(|_| {
            pace.after_failure();
            pace.failing()
        }).collect();
        assert_eq!(failing, [false, false, false, false, true, true]);
        // a read that works: from the start again
        pace.after_read();
        pace.after_failure();
        assert!(!pace.failing());
        assert_eq!(FAILED_AFTER, Duration::from_secs(30));
    }

    #[test]
    fn a_failed_read_marks_the_last_agenda() {
        let d = dir("failed");
        let mut link = Link::new(Arc::new(Store::at(&d)));
        // linked just now, not read yet: the screen learns that the read failed
        let not_read = failed(Some(&agenda_of(CalendarLink::CALENDAR_CONNECTED))).unwrap();
        assert_eq!((not_read.read_failed, not_read.updated, not_read.events.len()), (true, 0, 0));
        // read once: a failure keeps the events and the time of that read
        let first = link.connected(read(2), false, 1000, Instant::now());
        let after = failed(Some(&first)).unwrap();
        assert_eq!((after.read_failed, after.updated, after.events.len()), (true, 1000, 2));
        assert_eq!(after, Agenda { read_failed: true, ..first });
        // the next read clears it, and the kept agenda never has it
        assert!(!link.connected(read(2), false, 1060, Instant::now()).read_failed);
        assert!(!Agenda::parse_from_bytes(&fs::read(d.join(LAST_AGENDA)).unwrap()).unwrap().read_failed);
        // nothing to mark when the last agenda is not connected, or there is none
        assert_eq!(failed(Some(&agenda_of(CalendarLink::CALENDAR_EXPIRED))), None);
        assert_eq!(failed(None), None);
    }

    #[test]
    fn an_unchanged_agenda_is_not_written_every_minute() {
        let d = dir("keep");
        let mut link = Link::new(Arc::new(Store::at(&d)));
        let kept = || Agenda::parse_from_bytes(&fs::read(d.join(LAST_AGENDA)).unwrap()).unwrap().updated;
        let t0 = Instant::now();
        link.connected(read(2), false, 1000, t0);
        assert_eq!(kept(), 1000);
        // a minute later, the same events: published with the new time, not written
        assert_eq!(link.connected(read(2), false, 1060, t0 + Duration::from_secs(60)).updated, 1060);
        assert_eq!(kept(), 1000);
        // changed: written at once
        link.connected(read(3), false, 1120, t0 + Duration::from_secs(120));
        assert_eq!(kept(), 1120);
        // the same for an hour: written again
        link.connected(read(3), false, 4720, t0 + Duration::from_secs(120) + KEEP_EVERY);
        assert_eq!(kept(), 4720);
        // after an expiry the next read is written whatever it is
        link.revoked();
        link.connected(read(3), false, 4780, t0 + Duration::from_secs(180) + KEEP_EVERY);
        assert_eq!(kept(), 4780);
    }

    #[test]
    fn the_setup_url_for_development() {
        let d = dir("url");
        let link = Link::new(Arc::new(Store::at(&d)));
        link.setup_url("http://192.168.1.23:8765/setup?t=abc");
        assert_eq!(fs::read_to_string(d.join(SETUP_URL_FILE)).unwrap(), "http://192.168.1.23:8765/setup?t=abc\n");
        assert_eq!(mode(d.join(SETUP_URL_FILE)), 0o600);
        link.setup_url("");
        assert!(!d.join(SETUP_URL_FILE).exists());
        link.setup_url("");
    }

    const INVITE: &str = r#"{"etag": "\"7\"", "organizer": {"email": "boss@example.org"},
        "attendees": [{"email": "boss@example.org", "organizer": true, "responseStatus": "accepted"},
                      {"email": "anna@example.org", "self": true, "responseStatus": "needsAction"}]}"#;
    const MINE: &str = r#"{"etag": "\"8\"", "organizer": {"self": true}}"#;

    fn answer(response: Rsvp) -> Change {
        Change::Respond { event_id: "abc".into(), response }
    }

    fn remove() -> Change {
        Change::Delete { event_id: "abc".into() }
    }

    fn fresh(grant: Grant) -> Session {
        Session::new(client(), grant, Some(tokens("a1", None)), Instant::now())
    }

    fn failure(outcome: Result<(), Failure>) -> CalendarFailure {
        outcome.expect_err("a failure").kind
    }

    #[tokio::test]
    async fn an_answer_reads_the_event_then_patches_it() {
        let d = dir("answer");
        let store = Store::at(&d);
        let google = Script::api(vec![], vec![Ok((200, INVITE)), Ok((200, r#"{"id":"abc"}"#))]);
        assert_eq!(fresh(change_grant()).change(&google, &store, &answer(Rsvp::RSVP_TENTATIVE), Instant::now()).await, Ok(()));
        assert_eq!(google.calls(), ["GET abc a1", "PATCH abc a1 \"7\""]);
        let body: Value = serde_json::from_str(&google.bodies.lock().unwrap()[0]).unwrap();
        assert_eq!(body["attendees"][1]["responseStatus"], "tentative");
        assert_eq!(body["attendees"][0]["responseStatus"], "accepted");
        // the answer it has already: read, nothing patched
        let accepted = INVITE.replace("needsAction", "accepted");
        let google = Script::api(vec![], vec![Ok((200, accepted.as_str()))]);
        assert_eq!(fresh(change_grant()).change(&google, &store, &answer(Rsvp::RSVP_ACCEPTED), Instant::now()).await, Ok(()));
        assert_eq!(google.calls(), ["GET abc a1"]);
    }

    #[tokio::test]
    async fn a_deletion_reads_the_event_then_deletes_it() {
        let d = dir("delete");
        let store = Store::at(&d);
        let google = Script::api(vec![], vec![Ok((200, MINE)), Ok((204, ""))]);
        assert_eq!(fresh(change_grant()).change(&google, &store, &remove(), Instant::now()).await, Ok(()));
        assert_eq!(google.calls(), ["GET abc a1", "DELETE abc a1 \"8\""]);
        // gone before or in between
        let google = Script::api(vec![], vec![Ok((404, r#"{"error":{"errors":[{"reason":"notFound"}]}}"#))]);
        assert_eq!(failure(fresh(change_grant()).change(&google, &store, &remove(), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_GONE);
        let google = Script::api(vec![], vec![Ok((200, MINE)), Ok((410, r#"{"error":{"errors":[{"reason":"deleted"}]}}"#))]);
        assert_eq!(failure(fresh(change_grant()).change(&google, &store, &remove(), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_GONE);
        // an invitation is not the account's to delete: read, refused, nothing deleted
        let google = Script::api(vec![], vec![Ok((200, INVITE))]);
        assert_eq!(failure(fresh(change_grant()).change(&google, &store, &remove(), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED);
        assert_eq!(google.calls(), ["GET abc a1"]);
    }

    #[tokio::test]
    async fn a_401_renews_the_token_once_and_a_412_reads_again_once() {
        let d = dir("again401");
        let store = Store::at(&d);
        let unauthorized = r#"{"error":{"code":401}}"#;
        let google = Script::api(vec![Ok(tokens("a2", None))], vec![Ok((401, unauthorized)), Ok((200, MINE)), Ok((204, ""))]);
        assert_eq!(fresh(change_grant()).change(&google, &store, &remove(), Instant::now()).await, Ok(()));
        assert_eq!(google.calls(), ["GET abc a1", "refresh 1//r", "GET abc a2", "DELETE abc a2 \"8\""]);
        // refused again after the renewal: not a third time
        let google = Script::api(vec![Ok(tokens("a2", None))], vec![Ok((401, unauthorized)), Ok((401, unauthorized))]);
        assert_eq!(failure(fresh(change_grant()).change(&google, &store, &remove(), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_AGAIN);
        // the event changed between the read and the patch: read again, patched on the new version
        let changed = r#"{"error":{"code":412,"errors":[{"reason":"conditionNotMet"}]}}"#;
        let newer = INVITE.replace(r#"\"7\""#, r#"\"9\""#);
        let google = Script::api(vec![], vec![Ok((200, INVITE)), Ok((412, changed)), Ok((200, newer.as_str())), Ok((200, "{}"))]);
        assert_eq!(fresh(change_grant()).change(&google, &store, &answer(Rsvp::RSVP_DECLINED), Instant::now()).await, Ok(()));
        assert_eq!(google.calls(), ["GET abc a1", "PATCH abc a1 \"7\"", "GET abc a1", "PATCH abc a1 \"9\""]);
        // changing under the panel twice: later
        let google = Script::api(vec![], vec![Ok((200, INVITE)), Ok((412, changed)), Ok((200, INVITE)), Ok((412, changed))]);
        assert_eq!(failure(fresh(change_grant()).change(&google, &store, &answer(Rsvp::RSVP_DECLINED), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_AGAIN);
    }

    #[tokio::test]
    async fn a_change_that_fails_ends_in_a_clear_state() {
        let d = dir("fails");
        let store = Store::at(&d);
        // no network: later
        let google = Script::api(vec![], vec![Err("dns error")]);
        assert_eq!(failure(fresh(change_grant()).change(&google, &store, &remove(), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_AGAIN);
        let google = Script::api(vec![], vec![Ok((200, MINE)), Err("timeout")]);
        assert_eq!(failure(fresh(change_grant()).change(&google, &store, &remove(), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_AGAIN);
        let google = Script::api(vec![], vec![Ok((200, "<html>"))]);
        assert_eq!(failure(fresh(change_grant()).change(&google, &store, &remove(), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_AGAIN);
        // the grant revoked, the client refused: no change, the reads take it from there
        let google = Script::api(vec![Err(AuthError::Grant("invalid_grant".into()))], vec![]);
        let mut session = Session::new(client(), change_grant(), None, Instant::now());
        assert_eq!(failure(session.change(&google, &store, &remove(), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_NOT_LINKED);
        let google = Script::api(vec![Err(AuthError::Client("invalid_client".into()))], vec![]);
        assert_eq!(failure(session.change(&google, &store, &remove(), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_REFUSED);
        let google = Script::api(vec![Err(AuthError::Other("dns error".into()))], vec![]);
        assert_eq!(failure(session.change(&google, &store, &remove(), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_AGAIN);
        // Google refuses the change itself
        let google = Script::api(vec![], vec![Ok((200, INVITE)), Ok((403, r#"{"error":{"errors":[{"reason":"forbidden"}]}}"#))]);
        assert_eq!(failure(fresh(change_grant()).change(&google, &store, &answer(Rsvp::RSVP_ACCEPTED), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_REFUSED);
    }

    #[tokio::test]
    async fn google_saying_read_only_makes_the_grant_read_only() {
        let d = dir("readonly");
        let store = Store::at(&d);
        let mut session = fresh(change_grant());
        assert!(session.can_change());
        let scope = r#"{"error":{"code":403,"errors":[{"reason":"insufficientPermissions"}],"details":[{"reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT"}]}}"#;
        let google = Script::api(vec![], vec![Ok((200, MINE)), Ok((403, scope))]);
        assert_eq!(failure(session.change(&google, &store, &remove(), Instant::now()).await), CalendarFailure::CALENDAR_FAILURE_READ_ONLY);
        assert!(!session.can_change());
        // kept: the next start offers no change either
        let kept = Grant::parse(&fs::read(d.join(GRANT_FILE)).unwrap()).unwrap();
        assert_eq!((kept.access(), kept.refresh_token.as_str()), (Access::Read, "1//r"));
        assert_eq!(mode(d.join(GRANT_FILE)), 0o600);
    }

    #[tokio::test]
    async fn the_granted_scope_of_a_refresh_is_kept() {
        let d = dir("scope");
        let store = Store::at(&d);
        // a grant kept before the choice (no scope): read access until Google says more
        let mut session = Session::new(client(), Grant { scope: String::new(), ..grant() }, None, Instant::now());
        assert!(!session.can_change());
        let granted = Tokens { scope: Some(format!("{CHANGE_SCOPE} {READ_SCOPE}")), ..tokens("a1", None) };
        let google = Script::new(vec![Ok(granted)], vec![Ok(read(1))]);
        assert!(matches!(session.read(&google, &store, Instant::now(), 0).await, Outcome::Read(_)));
        assert!(session.can_change());
        assert_eq!(Grant::parse(&fs::read(d.join(GRANT_FILE)).unwrap()).unwrap().access(), Access::Change);
        // a refresh without a scope changes nothing
        let mut session = Session::new(client(), change_grant(), None, Instant::now());
        let google = Script::new(vec![Ok(tokens("a2", None))], vec![Ok(read(1))]);
        session.read(&google, &store, Instant::now(), 0).await;
        assert!(session.can_change());
    }

    #[tokio::test]
    async fn a_disconnect_ends_the_grant_at_google_then_forgets_it() {
        let d = dir("disconnect");
        let store = Arc::new(Store::at(&d));
        let mut link = Link::new(store.clone());
        let client = link.client().unwrap();
        link.linked(&change_grant());
        link.connected(read(3), true, 1000, Instant::now());
        let google = Script { revoke: Mutex::new(vec![Ok(())].into()), ..Script::default() };
        let mut session = fresh(change_grant());
        assert_eq!(session.disconnect(&google).await, Ok(()));
        assert_eq!(google.calls(), ["revoke 1//r"]);
        let agenda = link.disconnected();
        assert_eq!((link_of(&agenda), agenda.events.len(), agenda.can_change), (CalendarLink::CALENDAR_NOT_CONNECTED, 0, false));
        assert!(!d.join(GRANT_FILE).exists() && link.grant(&client).is_none());
        // a restart: not connected, not expired, no old events: the setup page with the choice
        assert_eq!(Link::new(store).last(), (false, None));
        assert_eq!(mode(d.join(LAST_AGENDA)), 0o600);
    }

    #[tokio::test]
    async fn a_disconnect_google_cannot_hear_keeps_the_account() {
        let d = dir("disconnectfails");
        let store = Arc::new(Store::at(&d));
        let mut link = Link::new(store.clone());
        let client = link.client().unwrap();
        link.linked(&grant());
        let google = Script { revoke: Mutex::new(vec![Err(AuthError::Other("dns error".into()))].into()), ..Script::default() };
        assert_eq!(failure(fresh(grant()).disconnect(&google).await), CalendarFailure::CALENDAR_FAILURE_AGAIN);
        // the bundle forgets the grant only after Google ended it
        assert_eq!(link.grant(&client), Some(grant()));
        // ended before (Google: invalid_token, mapped to Ok by parse_revoke_response): unlinked
        let google = Script { revoke: Mutex::new(vec![Ok(())].into()), ..Script::default() };
        assert_eq!(fresh(grant()).disconnect(&google).await, Ok(()));
    }

    #[tokio::test]
    async fn a_refresh_without_a_calendar_scope_ends_in_expired() {
        let d = dir("noscope");
        let store = Store::at(&d);
        let google = Script::new(vec![Ok(Tokens { scope: Some("openid email".into()), ..tokens("a1", None) })], vec![]);
        let outcome = Session::new(client(), grant(), None, Instant::now()).read(&google, &store, Instant::now(), 0).await;
        assert!(matches!(outcome, Outcome::Revoked(_)), "{outcome:?}");
        assert_eq!(google.calls(), ["refresh 1//r"]);
    }

    #[test]
    fn a_grant_of_a_replaced_client_is_found_to_be_ended() {
        let d = dir("replaced");
        let mut link = Link::new(Arc::new(Store::at(&d)));
        let client = link.client().unwrap();
        assert_eq!(link.grant_of_another_client(&client), None);
        link.linked(&grant());
        assert_eq!(link.grant_of_another_client(&client), None, "the client's own grant");
        let old = Grant { client_id: "id-0".into(), ..grant() };
        link.linked(&old);
        assert_eq!(link.grant_of_another_client(&client), Some(old));
        link.forget_grant();
        assert!(!d.join(GRANT_FILE).exists());
    }

    #[test]
    fn without_the_change_no_event_offers_one() {
        let d = dir("canchange");
        let mut link = Link::new(Arc::new(Store::at(&d)));
        let mut events = read(2);
        for e in &mut events.events {
            (e.can_respond, e.can_delete) = (true, true);
        }
        let agenda = link.connected(events.clone(), true, 1000, Instant::now());
        assert!(agenda.can_change && agenda.events.iter().all(|e| e.can_respond && e.can_delete));
        let agenda = link.connected(events, false, 1060, Instant::now());
        assert!(!agenda.can_change && agenda.events.iter().all(|e| !e.can_respond && !e.can_delete));
        // linked just now: the access granted, not read yet
        assert!(link.linked(&change_grant()).can_change);
        assert!(!link.linked(&grant()).can_change);
    }

    #[test]
    fn a_grant_of_another_client_is_not_taken() {
        let d = dir("other");
        let mut link = Link::new(Arc::new(Store::at(&d)));
        let client = link.client().unwrap();
        assert!(link.grant(&client).is_none());
        link.linked(&Grant { client_id: "id-0".into(), refresh_token: "1//old".into(), scope: String::new() });
        assert!(link.grant(&client).is_none());
        link.linked(&grant());
        assert_eq!(link.grant(&client), Some(grant()));
        fs::write(d.join(CLIENT_FILE), "{}").unwrap();
        assert!(link.client().is_err());
    }
}
