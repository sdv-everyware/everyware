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
//! refresh token kept) and what each outcome leaves in the bundle's directory and publishes
//! (`Link`). The bundle (../../src/lib.rs) loops, sleeps and publishes what these return.
//!
//! The outcomes and the screen:
//!
//! | outcome | the bundle's directory | the agenda |
//! |---|---|---|
//! | a read | `agenda.pb` | `CALENDAR_CONNECTED` with the events |
//! | `invalid_grant`, the scope gone | `grant.json` removed | `CALENDAR_EXPIRED`, no events, the setup page again |
//! | the client refused, the Calendar API off | `grant.json` kept (a reset secret keeps the client's id) | `CALENDAR_NO_CLIENT`, no events, until `oauth-client.json` changes |
//! | anything else | | the last agenda stays, with `read_failed` (`failed`); its `updated` tells its age |
//!
//! When the next read is (`Pace`): a minute after a read; after a failure 2 s, then twice as long
//! each time up to 5 minutes, so an outage of Google or of the network is not asked every minute.

use crate::events::{ApiError, Read};
use crate::google::Google;
use crate::oauth::{AuthError, ClientConfig, Grant, Tokens};
use everyware_bundle_store::Store;
use everyware_catalog::calendar::{Agenda, CalendarLink};
use protobuf::Message;
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
/// An agenda that did not change (a read every minute changes only `updated`) is written again
/// at most this often, to spare the flash; a changed one at once.
pub const KEEP_EVERY: Duration = Duration::from_secs(3600);
/// The setup URL for the development flow, in the bundle's directory (0600): the bundle's debug
/// lines do not reach logcat, and the URL holds a one-time token. Gone once the setup ends.
pub const SETUP_URL_FILE: &str = "setup-url";

/// When the next read is: `POLL_INTERVAL` after a read; after failures `RETRY_FIRST`, then twice
/// as long each time up to `RETRY_MAX`.
#[derive(Debug, Default)]
pub struct Pace {
    /// The last wait after a failure, while the reads fail.
    retry: Option<Duration>,
}

impl Pace {
    /// The wait after a read that succeeded; the failures start again from `RETRY_FIRST`.
    pub fn after_read(&mut self) -> Duration {
        self.retry = None;
        POLL_INTERVAL
    }

    /// The wait after a read that failed.
    pub fn after_failure(&mut self) -> Duration {
        let wait = self.retry.map_or(RETRY_FIRST, |last| (last * 2).min(RETRY_MAX));
        self.retry = Some(wait);
        wait
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

    /// The grant as it is now (a refresh may have brought a new refresh token).
    pub fn grant(&self) -> &Grant {
        &self.grant
    }

    /// One read at `now` (`unix`: the same as unix time): the access token renewed when it has
    /// less than `REFRESH_MARGIN` left, the events, and once more with a new access token when
    /// the API refused the one it had. A new refresh token goes into `store` at once.
    pub async fn read<G: Google + ?Sized>(&mut self, google: &G, store: &Store, now: Instant, unix: i64) -> Outcome {
        for _ in 0..2 {
            if self.access.as_ref().is_none_or(|(_, until)| now + REFRESH_MARGIN >= *until) {
                match google.refresh(&self.client, &self.grant.refresh_token).await {
                    Ok(tokens) => self.renewed(tokens, store, now),
                    Err(err @ (AuthError::Grant(_) | AuthError::Scope)) => return Outcome::Revoked(err.to_string()),
                    Err(err @ AuthError::Client(_)) => return Outcome::Refused(err.to_string()),
                    Err(err @ AuthError::Other(_)) => return Outcome::Again(err.to_string()),
                }
            }
            let token = self.access.as_ref().map(|(token, _)| token.clone()).unwrap_or_default();
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

    /// The tokens of a refresh: a new refresh token kept in place of the old.
    fn renewed(&mut self, tokens: Tokens, store: &Store, now: Instant) {
        if let Some(refresh_token) = tokens.refresh_token.filter(|t| *t != self.grant.refresh_token) {
            self.grant.refresh_token = refresh_token;
            if let Err(err) = store.save_private(GRANT_FILE, self.grant.to_json().as_bytes()) {
                log::error!("the new refresh token not kept: {err}");
            }
        }
        log::debug!("a new access token for {} s", tokens.expires_in);
        self.access = Some((tokens.access_token, now + Duration::from_secs(tokens.expires_in)));
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
        agenda_of(CalendarLink::CALENDAR_CONNECTED)
    }

    /// A read at `unix` (`now` on the monotonic clock): the connected agenda, kept when it changed
    /// or when the last one was kept `KEEP_EVERY` ago.
    pub fn connected(&mut self, read: Read, unix: i64, now: Instant) -> Agenda {
        let mut agenda = agenda_of(CalendarLink::CALENDAR_CONNECTED);
        agenda.calendar = read.calendar;
        agenda.events = read.events;
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

    /// The grant is not good any more: forgotten, and the events with it; expired, kept (it
    /// stays expired across restarts until an account is linked).
    pub fn revoked(&mut self) -> Agenda {
        if let Err(err) = self.store.remove(GRANT_FILE) {
            log::error!("the grant not removed: {err}");
        }
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
    use everyware_catalog::calendar::AgendaEvent;
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
        calls: Mutex<Vec<String>>,
    }

    impl Script {
        fn new(refresh: Vec<Result<Tokens, AuthError>>, events: Vec<Result<Read, ApiError>>) -> Script {
            Script { refresh: Mutex::new(refresh.into()), events: Mutex::new(events.into()), calls: Mutex::default() }
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

        async fn events(&self, access_token: &str, _: i64) -> Result<Read, ApiError> {
            self.calls.lock().unwrap().push(format!("events {access_token}"));
            self.events.lock().unwrap().pop_front().expect("scripted events")
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
        Grant { client_id: "id-1".into(), refresh_token: "1//r".into() }
    }

    fn tokens(access: &str, refresh: Option<&str>) -> Tokens {
        Tokens { access_token: access.into(), expires_in: 3600, refresh_token: refresh.map(str::to_string) }
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
        link.connected(read(3), 1_763_285_400, Instant::now());
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
        link.connected(read(3), 1_763_285_400, Instant::now());
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
        let agenda = link.connected(read(2), 1_763_285_400, Instant::now());
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
    fn a_failed_read_marks_the_last_agenda() {
        let d = dir("failed");
        let mut link = Link::new(Arc::new(Store::at(&d)));
        // linked just now, not read yet: the screen learns that the read failed
        let not_read = failed(Some(&agenda_of(CalendarLink::CALENDAR_CONNECTED))).unwrap();
        assert_eq!((not_read.read_failed, not_read.updated, not_read.events.len()), (true, 0, 0));
        // read once: a failure keeps the events and the time of that read
        let first = link.connected(read(2), 1000, Instant::now());
        let after = failed(Some(&first)).unwrap();
        assert_eq!((after.read_failed, after.updated, after.events.len()), (true, 1000, 2));
        assert_eq!(after, Agenda { read_failed: true, ..first });
        // the next read clears it, and the kept agenda never has it
        assert!(!link.connected(read(2), 1060, Instant::now()).read_failed);
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
        link.connected(read(2), 1000, t0);
        assert_eq!(kept(), 1000);
        // a minute later, the same events: published with the new time, not written
        assert_eq!(link.connected(read(2), 1060, t0 + Duration::from_secs(60)).updated, 1060);
        assert_eq!(kept(), 1000);
        // changed: written at once
        link.connected(read(3), 1120, t0 + Duration::from_secs(120));
        assert_eq!(kept(), 1120);
        // the same for an hour: written again
        link.connected(read(3), 4720, t0 + Duration::from_secs(120) + KEEP_EVERY);
        assert_eq!(kept(), 4720);
        // after an expiry the next read is written whatever it is
        link.revoked();
        link.connected(read(3), 4780, t0 + Duration::from_secs(180) + KEEP_EVERY);
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

    #[test]
    fn a_grant_of_another_client_is_not_taken() {
        let d = dir("other");
        let mut link = Link::new(Arc::new(Store::at(&d)));
        let client = link.client().unwrap();
        assert!(link.grant(&client).is_none());
        link.linked(&Grant { client_id: "id-0".into(), refresh_token: "1//old".into() });
        assert!(link.grant(&client).is_none());
        link.linked(&grant());
        assert_eq!(link.grant(&client), Some(grant()));
        fs::write(d.join(CLIENT_FILE), "{}").unwrap();
        assert!(link.client().is_err());
    }
}
