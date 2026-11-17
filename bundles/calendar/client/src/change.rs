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

//! The changes the screen may ask of the linked account's calendar (CalendarService,
//! catalog/calendar_service.proto): an answer to an invitation and the deletion of one of the
//! account's events, one occurrence of a recurring event each.
//!
//! A change passes four checks before Google hears of it: the caller is the dashboard bundle
//! (`caller_allowed`), the request is well formed (`Change::respond`, `Change::delete`: an id as
//! Google writes them, an answer), the agenda published last has the event and allows the change
//! (`Change::allowed_by`), and the event as Google has it now (events.get) still allows it
//! (`check`). The change is then made on that version only (If-Match with its ETag; when the
//! event changed in between, `Session::change` reads it again once). Google tells the others
//! (sendUpdates=all).
//!
//! The answer: events.patch of the attendees as Google gave them, with the account's own
//! responseStatus changed (a patch replaces the whole list), and `attendeesOmitted` when Google
//! left some out (then only the account's answer is changed). The deletion: events.delete.
//! <https://developers.google.com/workspace/calendar/api/v3/reference/events/patch>

use crate::events::{self, EVENTS_URL};
use everyware_catalog::calendar::{Agenda, CalendarLink, Rsvp};
use everyware_catalog::calendar_service::{CalendarDelete, CalendarFailure, CalendarRespond, CalendarResult};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::timeout;

/// The only caller: the dashboard bundle (bundles/dashboard/apex, its service bundles manifest).
const CALLER_PACKAGE: &str = "com.everyware.dashboard";
const CALLER_BUNDLE: &str = "DashboardServiceBundle";
/// The catalog's bound of `CalendarResult.error`, in bytes.
const MAX_ERROR: usize = 96;
/// An event with its attendees is a few kilobytes; a meeting of hundreds some hundred.
pub const MAX_REPLY: usize = 1024 * 1024;

/// Whether the SDV identity of a caller (package, bundle) may ask for changes. The authz policy
/// lets any bundle that declares itself a client call; only the screen's bridge asks.
pub fn caller_allowed(package: &str, bundle: &str) -> bool {
    package == CALLER_PACKAGE && bundle == CALLER_BUNDLE
}

/// A change the screen asked for, well formed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// The account's answer to the invitation of `event_id`: accepted, tentative or declined.
    Respond { event_id: String, response: Rsvp },
    /// The deletion of the account's event `event_id`.
    Delete { event_id: String },
}

/// Why a change was not made: what the screen says (`kind`) and the words for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub kind: CalendarFailure,
    pub why: String,
}

impl Failure {
    pub fn new(kind: CalendarFailure, why: impl Into<String>) -> Failure {
        Failure { kind, why: why.into() }
    }

    /// Something that may work later: the network, Google busy, no answer in time.
    pub fn again(why: impl Into<String>) -> Failure {
        Failure::new(CalendarFailure::CALENDAR_FAILURE_AGAIN, why)
    }

    fn not_allowed(why: impl Into<String>) -> Failure {
        Failure::new(CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED, why)
    }

    fn gone(why: impl Into<String>) -> Failure {
        Failure::new(CalendarFailure::CALENDAR_FAILURE_GONE, why)
    }
}

/// Where the answer to what the screen asked goes, on the bundle's loop's side: first the word
/// that the loop took it, then the answer. The service's side waits for both (`Waiting`).
pub struct Reply {
    taken: Option<oneshot::Sender<()>>,
    result: oneshot::Sender<CalendarResult>,
}

/// The service's side of a `Reply`.
pub struct Waiting {
    took: oneshot::Receiver<()>,
    answer: oneshot::Receiver<CalendarResult>,
}

impl Reply {
    /// A reply and what waits for it.
    pub fn new() -> (Reply, Waiting) {
        let (taken, took) = oneshot::channel();
        let (result, answer) = oneshot::channel();
        (Reply { taken: Some(taken), result }, Waiting { took, answer })
    }

    /// The loop takes it. False when nobody waits for it any more (the service gave up waiting
    /// for the loop): then it is not made, so that no change is made that the screen was told
    /// failed.
    pub fn take(&mut self) -> bool {
        self.taken.take().is_some_and(|taken| taken.send(()).is_ok())
    }

    /// The answer (taken first, when it was not yet).
    pub fn send(mut self, result: CalendarResult) {
        let _ = self.take();
        let _ = self.result.send(result);
    }
}

impl Waiting {
    /// The answer: the loop takes it within `take_wait` (or it is not made: "busy, nothing was
    /// changed"), and answers within `answer_wait` from the moment it took it, so a slow read the
    /// loop was in does not eat the time of the change.
    pub async fn answer(self, take_wait: Duration, answer_wait: Duration) -> CalendarResult {
        let again = |why: &str| result(&Err(Failure::again(why)));
        // dropped when this gives up: the loop then does not make it (Reply::take)
        match timeout(take_wait, self.took).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return again("the calendar stopped"),
            Err(_) => return again("the calendar is busy; nothing was changed"),
        }
        match timeout(answer_wait, self.answer).await {
            Ok(Ok(answer)) => answer,
            _ => again("no answer in time"),
        }
    }
}

/// The answer of the service for an outcome.
pub fn result(outcome: &Result<(), Failure>) -> CalendarResult {
    let mut result = CalendarResult::new();
    match outcome {
        Ok(()) => result.ok = true,
        Err(failure) => {
            result.failure = failure.kind.into();
            result.error = events::text(&failure.why, MAX_ERROR);
        }
    }
    result
}

impl Change {
    /// An answer as the screen asked for it: an id as Google writes them and one of the three
    /// answers.
    pub fn respond(request: &CalendarRespond) -> Result<Change, Failure> {
        let event_id = checked_id(&request.event_id)?;
        let response = request.response.enum_value_or_default();
        match response {
            Rsvp::RSVP_ACCEPTED | Rsvp::RSVP_TENTATIVE | Rsvp::RSVP_DECLINED => Ok(Change::Respond { event_id, response }),
            _ => Err(Failure::not_allowed("no answer given")),
        }
    }

    /// A deletion as the screen asked for it.
    pub fn delete(request: &CalendarDelete) -> Result<Change, Failure> {
        Ok(Change::Delete { event_id: checked_id(&request.event_id)? })
    }

    /// The event the change is for.
    pub fn event_id(&self) -> &str {
        match self {
            Change::Respond { event_id, .. } | Change::Delete { event_id } => event_id,
        }
    }

    /// Whether the agenda published last allows the change: an account linked with write access,
    /// the event in the agenda, the event allows it (`AgendaEvent.can_respond`, `can_delete`).
    pub fn allowed_by(&self, agenda: Option<&Agenda>) -> Result<(), Failure> {
        let agenda = agenda.filter(|a| a.link.enum_value_or_default() == CalendarLink::CALENDAR_CONNECTED);
        let Some(agenda) = agenda else {
            return Err(Failure::new(CalendarFailure::CALENDAR_FAILURE_NOT_LINKED, "no account is linked"));
        };
        if !agenda.can_change {
            return Err(Failure::new(CalendarFailure::CALENDAR_FAILURE_READ_ONLY, "the panel may only read the calendar"));
        }
        let Some(event) = agenda.events.iter().find(|e| e.id == self.event_id()) else {
            return Err(Failure::gone("the event is not in the agenda"));
        };
        match self {
            Change::Respond { .. } if !event.can_respond => Err(Failure::not_allowed("not an invitation the panel may answer")),
            Change::Delete { .. } if !event.can_delete => Err(Failure::not_allowed("not an event the panel may delete")),
            _ => Ok(()),
        }
    }

    /// The read of the event as Google has it now: what `check` and the change need.
    pub fn get(&self) -> ApiCall {
        let fields = match self {
            // every attendee with every field: the patch gives the list back whole
            Change::Respond { .. } => "etag,status,eventType,visibility,organizer(self),attendeesOmitted,attendees",
            Change::Delete { .. } => "etag,status,eventType,visibility,organizer(self)",
        };
        let query = form_urlencoded::Serializer::new(String::new()).append_pair("fields", fields).finish();
        ApiCall { method: "GET", url: format!("{}?{query}", event_url(self.event_id())), if_match: None, body: None }
    }

    /// The call that makes the change on the event Google gave (`item`, as `get` read it), or
    /// None when there is nothing to change (the answer is the one asked for already).
    pub fn call(&self, item: &Value) -> Result<Option<ApiCall>, Failure> {
        check(self, item)?;
        let if_match = item["etag"].as_str().map(str::to_string);
        match self {
            Change::Respond { event_id, response } => {
                let Some(body) = respond_body(item, *response)? else { return Ok(None) };
                let query = form_urlencoded::Serializer::new(String::new()).append_pair("sendUpdates", "all").append_pair("fields", "id").finish();
                Ok(Some(ApiCall { method: "PATCH", url: format!("{}?{query}", event_url(event_id)), if_match, body: Some(body) }))
            }
            Change::Delete { event_id } => {
                let query = form_urlencoded::Serializer::new(String::new()).append_pair("sendUpdates", "all").finish();
                Ok(Some(ApiCall { method: "DELETE", url: format!("{}?{query}", event_url(event_id)), if_match, body: None }))
            }
        }
    }
}

/// `id` when it is an event id as Google writes them (events::valid_id): only such an id goes
/// into a URL.
fn checked_id(id: &str) -> Result<String, Failure> {
    if events::valid_id(id) {
        Ok(id.to_string())
    } else {
        Err(Failure::not_allowed("not an event id"))
    }
}

/// The URL of one event of the primary calendar. The id is checked (`checked_id`): letters,
/// digits, `_` and `-` only, nothing to escape.
fn event_url(id: &str) -> String {
    format!("{EVENTS_URL}/{id}")
}

/// One call of the Calendar API for a change, with the access token added by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiCall {
    /// "GET", "PATCH" or "DELETE".
    pub method: &'static str,
    pub url: String,
    /// The ETag the event must still have (412 when it changed since).
    pub if_match: Option<String>,
    /// JSON.
    pub body: Option<String>,
}

/// The rule of the agenda (`events::may_respond`, `may_delete`) on the event as Google has it
/// now: it may have changed since the agenda was read.
pub fn check(change: &Change, item: &Value) -> Result<(), Failure> {
    if item["status"] == "cancelled" {
        return Err(Failure::gone("the event was cancelled"));
    }
    let allowed = match change {
        Change::Respond { .. } => events::may_respond(item),
        Change::Delete { .. } => events::may_delete(item),
    };
    if allowed {
        Ok(())
    } else {
        Err(Failure::not_allowed("the event does not allow this change any more"))
    }
}

/// Google's word for an answer.
fn status_of(response: Rsvp) -> &'static str {
    match response {
        Rsvp::RSVP_ACCEPTED => "accepted",
        Rsvp::RSVP_TENTATIVE => "tentative",
        Rsvp::RSVP_DECLINED => "declined",
        Rsvp::RSVP_NONE | Rsvp::RSVP_NEEDS_ACTION => "needsAction",
    }
}

/// The body of the patch that answers: the attendees as Google gave them (a patch replaces the
/// whole list), the account's own entry with its new responseStatus, and `attendeesOmitted` when
/// Google left some out (the patch then changes only the account's answer). None when the answer
/// is the one asked for already.
pub fn respond_body(item: &Value, response: Rsvp) -> Result<Option<String>, Failure> {
    let status = status_of(response);
    let mut attendees = item["attendees"].as_array().cloned().unwrap_or_default();
    let Some(own) = attendees.iter_mut().find(|a| a["self"] == true) else {
        return Err(Failure::not_allowed("the account is not a guest"));
    };
    if own["responseStatus"] == status {
        return Ok(None);
    }
    own["responseStatus"] = Value::from(status);
    let mut body = serde_json::json!({ "attendees": attendees });
    if item["attendeesOmitted"] == true {
        body["attendeesOmitted"] = Value::Bool(true);
    }
    Ok(Some(body.to_string()))
}

/// What an answer of the API that is not a success means for a change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// 401: the access token is no longer good (renew it and try once more).
    Unauthorized,
    /// 412: the event changed since it was read (read it again and try once more).
    Changed,
    /// Anything else: the change is not made.
    Failed(Failure),
}

/// An answer of the API to a call of a change that is not a success, by its status and its
/// error's reasons.
pub fn refusal(status: u16, body: &[u8]) -> Refusal {
    let (message, reasons) = events::error_of(body);
    let has = |names: &[&str]| reasons.iter().any(|r| names.contains(&r.as_str()));
    let first = reasons.first().map_or(message.as_str(), String::as_str);
    let why = format!("Google: HTTP {status} {first}");
    let failed = |kind| Refusal::Failed(Failure::new(kind, why.clone()));
    match status {
        401 => Refusal::Unauthorized,
        412 => Refusal::Changed,
        404 | 410 => failed(CalendarFailure::CALENDAR_FAILURE_GONE),
        403 if has(&["insufficientPermissions", "ACCESS_TOKEN_SCOPE_INSUFFICIENT"]) => failed(CalendarFailure::CALENDAR_FAILURE_READ_ONLY),
        403 | 429 if has(&["rateLimitExceeded", "userRateLimitExceeded", "quotaExceeded", "RATE_LIMIT_EXCEEDED"]) => failed(CalendarFailure::CALENDAR_FAILURE_AGAIN),
        429 | 500..=599 => failed(CalendarFailure::CALENDAR_FAILURE_AGAIN),
        // the API turned off, a forbidden change, a request Google does not take
        400..=499 => failed(CalendarFailure::CALENDAR_FAILURE_REFUSED),
        _ => failed(CalendarFailure::CALENDAR_FAILURE_AGAIN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyware_catalog::calendar::AgendaEvent;
    use std::collections::HashMap;

    fn respond(id: &str, response: Rsvp) -> CalendarRespond {
        let mut r = CalendarRespond::new();
        r.event_id = id.into();
        r.response = response.into();
        r
    }

    fn delete(id: &str) -> CalendarDelete {
        let mut r = CalendarDelete::new();
        r.event_id = id.into();
        r
    }

    fn kind(r: Result<impl std::fmt::Debug, Failure>) -> CalendarFailure {
        r.expect_err("a failure").kind
    }

    #[test]
    fn only_the_dashboard_bundle_asks() {
        assert!(caller_allowed("com.everyware.dashboard", "DashboardServiceBundle"));
        for (package, bundle) in [("com.everyware.news", "DashboardServiceBundle"), ("com.everyware.dashboard", "NewsServiceBundle"), ("", ""), ("com.everyware.dashboard ", "DashboardServiceBundle")] {
            assert!(!caller_allowed(package, bundle), "{package}.{bundle}");
        }
    }

    #[test]
    fn requests_from_the_screen_are_checked() {
        assert_eq!(Change::respond(&respond("abc_20261116T080000Z", Rsvp::RSVP_TENTATIVE)), Ok(Change::Respond { event_id: "abc_20261116T080000Z".into(), response: Rsvp::RSVP_TENTATIVE }));
        assert_eq!(Change::delete(&delete("abc")), Ok(Change::Delete { event_id: "abc".into() }));
        // ids that are not Google's never reach a URL: a path, a query, a fragment, escapes, too long
        let long = "a".repeat(129);
        for bad in ["", "../calendars", "abc/../../users/me", "abc?sendUpdates=none", "abc#x", "abc%2F", "a b", "ä", long.as_str()] {
            assert_eq!(kind(Change::delete(&delete(bad))), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED, "{bad}");
            assert_eq!(kind(Change::respond(&respond(bad, Rsvp::RSVP_ACCEPTED))), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED, "{bad}");
        }
        // an answer is one of the three
        for none in [Rsvp::RSVP_NONE, Rsvp::RSVP_NEEDS_ACTION] {
            assert_eq!(kind(Change::respond(&respond("abc", none))), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED);
        }
        let mut unknown = respond("abc", Rsvp::RSVP_ACCEPTED);
        unknown.response = protobuf::EnumOrUnknown::from_i32(99);
        assert_eq!(kind(Change::respond(&unknown)), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED);
    }

    fn agenda(can_change: bool) -> Agenda {
        let event = |id: &str, can_respond: bool, can_delete: bool| {
            let mut e = AgendaEvent::new();
            e.id = id.into();
            e.can_respond = can_respond;
            e.can_delete = can_delete;
            e
        };
        let mut a = Agenda::new();
        a.link = CalendarLink::CALENDAR_CONNECTED.into();
        a.can_change = can_change;
        a.events = vec![event("invite", true, false), event("mine", false, true), event("other", false, false)];
        a
    }

    #[test]
    fn the_agenda_published_last_decides_first() {
        let answer = |id: &str| Change::Respond { event_id: id.into(), response: Rsvp::RSVP_ACCEPTED };
        let remove = |id: &str| Change::Delete { event_id: id.into() };
        let a = agenda(true);
        assert_eq!(answer("invite").allowed_by(Some(&a)), Ok(()));
        assert_eq!(remove("mine").allowed_by(Some(&a)), Ok(()));
        assert_eq!(kind(answer("mine").allowed_by(Some(&a))), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED);
        assert_eq!(kind(remove("invite").allowed_by(Some(&a))), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED);
        assert_eq!(kind(remove("other").allowed_by(Some(&a))), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED);
        assert_eq!(kind(remove("gone").allowed_by(Some(&a))), CalendarFailure::CALENDAR_FAILURE_GONE);
        // read access only: nothing, whatever an event says
        assert_eq!(kind(remove("mine").allowed_by(Some(&agenda(false)))), CalendarFailure::CALENDAR_FAILURE_READ_ONLY);
        // no account
        assert_eq!(kind(remove("mine").allowed_by(None)), CalendarFailure::CALENDAR_FAILURE_NOT_LINKED);
        let mut expired = agenda(true);
        expired.link = CalendarLink::CALENDAR_EXPIRED.into();
        assert_eq!(kind(remove("mine").allowed_by(Some(&expired))), CalendarFailure::CALENDAR_FAILURE_NOT_LINKED);
    }

    fn query(url: &str) -> HashMap<String, String> {
        form_urlencoded::parse(url.split_once('?').unwrap().1.as_bytes()).into_owned().collect()
    }

    const INVITE: &str = r#"{"etag": "\"3181161784712000\"", "status": "confirmed", "organizer": {"email": "boss@example.org"},
        "attendees": [{"email": "boss@example.org", "organizer": true, "responseStatus": "accepted"},
                      {"email": "anna@example.org", "self": true, "responseStatus": "needsAction", "comment": "maybe late"},
                      {"email": "room@resource.calendar.google.com", "resource": true, "responseStatus": "accepted"}]}"#;

    #[test]
    fn an_answer_changes_the_accounts_attendee_only() {
        let change = Change::Respond { event_id: "abc_20261116T080000Z".into(), response: Rsvp::RSVP_ACCEPTED };
        let get = change.get();
        assert_eq!((get.method, get.if_match.as_deref(), get.body.as_deref()), ("GET", None, None));
        assert!(get.url.starts_with(&format!("{EVENTS_URL}/abc_20261116T080000Z?")), "{}", get.url);
        assert_eq!(query(&get.url)["fields"], "etag,status,eventType,visibility,organizer(self),attendeesOmitted,attendees");

        let item: Value = serde_json::from_str(INVITE).unwrap();
        let patch = change.call(&item).unwrap().unwrap();
        assert_eq!((patch.method, patch.if_match.as_deref()), ("PATCH", Some("\"3181161784712000\"")));
        assert!(patch.url.starts_with(&format!("{EVENTS_URL}/abc_20261116T080000Z?")));
        let q = query(&patch.url);
        assert_eq!((q["sendUpdates"].as_str(), q["fields"].as_str()), ("all", "id"));
        let body: Value = serde_json::from_str(patch.body.as_deref().unwrap()).unwrap();
        // the whole list back, only the account's answer changed, its comment kept
        let mut expected: Value = serde_json::from_str(INVITE).unwrap();
        expected["attendees"][1]["responseStatus"] = "accepted".into();
        assert_eq!(body, serde_json::json!({ "attendees": expected["attendees"] }));
        // Google left some guests out: it is told so, and changes only the account's answer
        let mut omitted = item.clone();
        omitted["attendeesOmitted"] = true.into();
        omitted["attendees"] = serde_json::json!([{"email": "anna@example.org", "self": true, "responseStatus": "needsAction"}]);
        let body: Value = serde_json::from_str(&change.call(&omitted).unwrap().unwrap().body.unwrap()).unwrap();
        assert_eq!(body, serde_json::json!({"attendees": [{"email": "anna@example.org", "self": true, "responseStatus": "accepted"}], "attendeesOmitted": true}));
        // the answer it has already: nothing to change
        let mut accepted = item.clone();
        accepted["attendees"][1]["responseStatus"] = "accepted".into();
        assert_eq!(change.call(&accepted), Ok(None));
    }

    #[test]
    fn the_event_as_google_has_it_decides_last() {
        let answer = Change::Respond { event_id: "abc".into(), response: Rsvp::RSVP_DECLINED };
        let remove = Change::Delete { event_id: "abc".into() };
        let item = |json: &str| serde_json::from_str::<Value>(json).unwrap();
        let mine = item(r#"{"etag": "\"1\"", "organizer": {"self": true}, "attendees": [{"self": true, "organizer": true, "responseStatus": "accepted"}]}"#);
        // the organizer deletes, does not answer
        let call = remove.call(&mine).unwrap().unwrap();
        assert_eq!((call.method, call.if_match.as_deref(), call.body), ("DELETE", Some("\"1\""), None));
        assert_eq!(query(&call.url)["sendUpdates"], "all");
        assert_eq!(query(&remove.get().url)["fields"], "etag,status,eventType,visibility,organizer(self)");
        assert_eq!(kind(answer.call(&mine)), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED);
        // a guest answers, does not delete
        assert_eq!(kind(remove.call(&item(INVITE))), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED);
        // since the agenda was read: cancelled, made private, the guest removed
        assert_eq!(kind(remove.call(&item(r#"{"status": "cancelled", "organizer": {"self": true}}"#))), CalendarFailure::CALENDAR_FAILURE_GONE);
        assert_eq!(kind(remove.call(&item(r#"{"visibility": "private", "organizer": {"self": true}}"#))), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED);
        assert_eq!(kind(answer.call(&item(r#"{"organizer": {"email": "a@example.org"}, "attendees": [{"email": "b@example.org"}]}"#))), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED);
        // not an ordinary event
        assert_eq!(kind(remove.call(&item(r#"{"eventType": "birthday", "organizer": {"self": true}}"#))), CalendarFailure::CALENDAR_FAILURE_NOT_ALLOWED);
    }

    #[test]
    fn googles_refusals() {
        let failed = |status, body: &str| match refusal(status, body.as_bytes()) {
            Refusal::Failed(f) => f.kind,
            other => panic!("{other:?}"),
        };
        assert_eq!(refusal(401, b"{}"), Refusal::Unauthorized);
        assert_eq!(refusal(412, br#"{"error":{"code":412,"message":"Precondition Failed","errors":[{"reason":"conditionNotMet"}]}}"#), Refusal::Changed);
        // the grant is read only, whatever the agenda said
        let scope = r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.","errors":[{"reason":"insufficientPermissions"}],
            "details":[{"reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT"}]}}"#;
        assert_eq!(failed(403, scope), CalendarFailure::CALENDAR_FAILURE_READ_ONLY);
        assert_eq!(failed(404, r#"{"error":{"code":404,"message":"Not Found","errors":[{"reason":"notFound"}]}}"#), CalendarFailure::CALENDAR_FAILURE_GONE);
        assert_eq!(failed(410, r#"{"error":{"code":410,"message":"Resource has been deleted","errors":[{"reason":"deleted"}]}}"#), CalendarFailure::CALENDAR_FAILURE_GONE);
        assert_eq!(failed(403, r#"{"error":{"errors":[{"reason":"forbiddenForNonOrganizer"}]}}"#), CalendarFailure::CALENDAR_FAILURE_REFUSED);
        assert_eq!(failed(403, r#"{"error":{"details":[{"reason":"SERVICE_DISABLED"}]}}"#), CalendarFailure::CALENDAR_FAILURE_REFUSED);
        assert_eq!(failed(400, r#"{"error":{"errors":[{"reason":"invalid"}]}}"#), CalendarFailure::CALENDAR_FAILURE_REFUSED);
        // a quota, Google busy: later
        assert_eq!(failed(403, r#"{"error":{"errors":[{"reason":"rateLimitExceeded"}]}}"#), CalendarFailure::CALENDAR_FAILURE_AGAIN);
        assert_eq!(failed(429, "{}"), CalendarFailure::CALENDAR_FAILURE_AGAIN);
        assert_eq!(failed(503, "<html>"), CalendarFailure::CALENDAR_FAILURE_AGAIN);
        // the words for the log name the reason, within the catalog's bound
        let Refusal::Failed(f) = refusal(403, br#"{"error":{"errors":[{"reason":"forbiddenForNonOrganizer"}]}}"#) else { panic!() };
        assert_eq!(f.why, "Google: HTTP 403 forbiddenForNonOrganizer");
        let long = refusal(400, format!(r#"{{"error":{{"message":"{}"}}}}"#, "x".repeat(500)).as_bytes());
        let Refusal::Failed(f) = long else { panic!() };
        assert!(result(&Err(f)).error.len() <= MAX_ERROR);
    }

    const SHORT: Duration = Duration::from_millis(100);

    #[tokio::test]
    async fn a_change_the_loop_does_not_take_in_time_is_not_made() {
        let (mut reply, waiting) = Reply::new();
        let answer = waiting.answer(SHORT, SHORT).await;
        assert_eq!((answer.ok, answer.failure.enum_value_or_default(), answer.error.as_str()), (false, CalendarFailure::CALENDAR_FAILURE_AGAIN, "the calendar is busy; nothing was changed"));
        assert!(!reply.take(), "nobody waits: the loop does not make it");
    }

    #[tokio::test]
    async fn the_time_for_the_answer_starts_when_the_loop_takes_it() {
        let (mut reply, waiting) = Reply::new();
        let service = tokio::spawn(waiting.answer(SHORT * 4, SHORT * 4));
        // the loop is busy for most of the first wait, then takes it and needs most of the second
        tokio::time::sleep(SHORT * 3).await;
        assert!(reply.take());
        tokio::time::sleep(SHORT * 3).await;
        reply.send(result(&Ok(())));
        assert!(service.await.unwrap().ok, "the change Google made is answered as made");
        // taken, and no answer in time
        let (mut reply, waiting) = Reply::new();
        assert!(reply.take());
        let answer = waiting.answer(SHORT, SHORT).await;
        assert_eq!(answer.error, "no answer in time");
        // an answer without a take (no account: refused at once) goes through
        let (reply, waiting) = Reply::new();
        reply.send(result(&Err(Failure::new(CalendarFailure::CALENDAR_FAILURE_NOT_LINKED, "no account"))));
        assert_eq!(waiting.answer(SHORT, SHORT).await.failure.enum_value_or_default(), CalendarFailure::CALENDAR_FAILURE_NOT_LINKED);
    }

    #[test]
    fn the_answers_of_the_service() {
        let ok = result(&Ok(()));
        assert!(ok.ok && ok.failure.enum_value_or_default() == CalendarFailure::CALENDAR_FAILURE_NONE && ok.error.is_empty());
        let failed = result(&Err(Failure::again("no answer\nin time")));
        assert!(!failed.ok);
        assert_eq!((failed.failure.enum_value_or_default(), failed.error.as_str()), (CalendarFailure::CALENDAR_FAILURE_AGAIN, "no answer in time"));
    }
}
