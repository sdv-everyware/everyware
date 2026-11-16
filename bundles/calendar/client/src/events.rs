//! The Calendar API's Events:list of the primary calendar, read for the agenda: the next seven
//! days, recurring events as their occurrences, in Google's order (by start in the calendar's
//! time zone), with only the fields the agenda needs, without the invitations the owner declined.
//! <https://developers.google.com/workspace/calendar/api/v3/reference/events/list>

use crate::time::{self, DAY};
use everyware_catalog::calendar::AgendaEvent;
use serde_json::Value;

/// Events:list of the account's primary calendar.
pub const EVENTS_URL: &str = "https://www.googleapis.com/calendar/v3/calendars/primary/events";
/// The days the agenda looks ahead.
pub const DAYS: i64 = 7;
/// The events of an agenda, at most (the catalog's bound).
pub const MAX_EVENTS: usize = 40;
/// Only what the agenda needs; of the attendees only the account's own answer (with
/// `maxAttendees=1` Google lists only the account itself).
pub const FIELDS: &str = "summary,items(id,status,summary,location,visibility,colorId,start,end,attendees(self,responseStatus))";
/// The title of a private or confidential event.
pub const BUSY: &str = "Busy";
/// The catalog's bounds (catalog/calendar.proto), in bytes.
const MAX_CALENDAR: usize = 64;
const MAX_ID: usize = 128;
const MAX_TITLE: usize = 256;
const MAX_LOCATION: usize = 128;
/// 40 events with the fields above are some 20 kB.
const MAX_REPLY: usize = 1024 * 1024;

/// Google's event palette (the `event` colours of the Colors API), by `colorId` 1 to 11.
const EVENT_COLORS: [u32; 11] = [0xa4bdfc, 0x7ae7bf, 0xdbadff, 0xff887c, 0xfbd75b, 0xffb878, 0x46d6db, 0xe1e1e1, 0x5484ed, 0x51b749, 0xdc2127];

/// What a read of the calendar gave.
#[derive(Clone, Debug, PartialEq)]
pub struct Read {
    /// The calendar's title (Events:list `summary`).
    pub calendar: String,
    /// Its events from now on, in Google's order, at most `MAX_EVENTS`.
    pub events: Vec<AgendaEvent>,
}

/// Why a read failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiError {
    /// 401: the access token is no longer good (refresh it and try again).
    Unauthorized,
    /// 403, the Calendar API is turned off in the OAuth client's project (`accessNotConfigured`,
    /// `SERVICE_DISABLED`): it stays so until the owner turns it on.
    Disabled(String),
    /// 403, the access token does not carry the calendar scope: the account has to be linked again.
    Scope(String),
    /// Anything else (a quota, the network, Google): try again later.
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized => f.write_str("the access token was refused"),
            ApiError::Disabled(why) => write!(f, "the Calendar API is turned off in the client's project ({why})"),
            ApiError::Scope(why) => write!(f, "the access token lacks the calendar scope ({why})"),
            ApiError::Other(why) => f.write_str(why),
        }
    }
}

/// The request of the events from `now` (unix time) to `DAYS` days later.
pub fn events_url(now: i64) -> String {
    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("timeMin", &time::rfc3339_utc(now))
        .append_pair("timeMax", &time::rfc3339_utc(now + DAYS * DAY))
        .append_pair("singleEvents", "true")
        .append_pair("orderBy", "startTime")
        .append_pair("maxResults", &MAX_EVENTS.to_string())
        .append_pair("maxAttendees", "1")
        .append_pair("fields", FIELDS)
        .finish();
    format!("{EVENTS_URL}?{query}")
}

/// The calendar's events from `now` on, with the access token `token`.
pub async fn read(token: &str, now: i64) -> Result<Read, ApiError> {
    let reply = everyware_https::get_bearer(&events_url(now), token, MAX_REPLY).await.map_err(|e| ApiError::Other(e.to_string()))?;
    match reply.status {
        200 => parse_events(&String::from_utf8_lossy(&reply.body)).map_err(ApiError::Other),
        status => Err(api_error(status, &reply.body)),
    }
}

/// An answer of the API that is not a success, by its status and its error's reasons
/// (`{"error": {"message", "errors": [{"reason"}], "details": [{"reason"}]}}`).
pub fn api_error(status: u16, body: &[u8]) -> ApiError {
    let v: Value = serde_json::from_slice(body).unwrap_or_default();
    let error = &v["error"];
    let message = format!("Events:list: HTTP {status} {}", text(error["message"].as_str().unwrap_or(""), 160));
    let reasons: Vec<&str> = ["errors", "details"]
        .iter()
        .filter_map(|key| error[*key].as_array())
        .flatten()
        .filter_map(|e| e["reason"].as_str())
        .collect();
    let has = |names: &[&str]| reasons.iter().any(|r| names.contains(r));
    match status {
        401 => ApiError::Unauthorized,
        403 if has(&["accessNotConfigured", "SERVICE_DISABLED"]) => ApiError::Disabled(message),
        403 if has(&["insufficientPermissions", "ACCESS_TOKEN_SCOPE_INSUFFICIENT"]) => ApiError::Scope(message),
        _ => ApiError::Other(message),
    }
}

/// An Events:list answer as the agenda's events: cancelled ones and the invitations the account
/// declined left out, a private or confidential one as "Busy" without its place, an all-day one
/// with its dates at 00:00 UTC, an event without a start left out; at most `MAX_EVENTS`.
///
/// The order is Google's (`orderBy=startTime`: by start in the calendar's time zone, an all-day
/// event at the start of its day there). It is not sorted again here: an all-day event's 00:00 UTC
/// is not the start of its day on the panel (in New York it is 19:00 the day before).
pub fn parse_events(json: &str) -> Result<Read, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("Events:list: not JSON: {e}"))?;
    let items = match v.get("items") {
        Some(Value::Array(items)) => items.as_slice(),
        None => &[],
        Some(_) => return Err("Events:list: items is not a list".into()),
    };
    let mut events: Vec<AgendaEvent> = items.iter().filter_map(event).collect();
    events.truncate(MAX_EVENTS);
    Ok(Read { calendar: text(v["summary"].as_str().unwrap_or(""), MAX_CALENDAR), events })
}

/// One item of the list, or None for one the agenda does not show.
fn event(item: &Value) -> Option<AgendaEvent> {
    if item["status"] == "cancelled" || declined(item) {
        return None;
    }
    let (start, all_day) = when(&item["start"])?;
    let end = match when(&item["end"]) {
        Some((end, _)) if end >= start => end,
        _ if all_day => start + DAY,
        _ => start,
    };
    let private = matches!(item["visibility"].as_str(), Some("private" | "confidential"));
    let mut e = AgendaEvent::new();
    e.id = text(item["id"].as_str().unwrap_or(""), MAX_ID);
    e.title = if private { BUSY.to_string() } else { text(item["summary"].as_str().unwrap_or(""), MAX_TITLE) };
    e.location = if private { String::new() } else { text(item["location"].as_str().unwrap_or(""), MAX_LOCATION) };
    e.start = start;
    e.end = end;
    e.all_day = all_day;
    e.color = item["colorId"].as_str().and_then(|c| c.parse::<usize>().ok()).and_then(|c| EVENT_COLORS.get(c.wrapping_sub(1))).copied().unwrap_or(0);
    Some(e)
}

/// The account declined the invitation (its own attendee entry says so).
fn declined(item: &Value) -> bool {
    item["attendees"].as_array().is_some_and(|all| all.iter().any(|a| a["self"] == true && a["responseStatus"] == "declined"))
}

/// A start or an end: `{"date": "2026-11-16"}` (all day, 00:00 UTC) or `{"dateTime": RFC 3339}`.
fn when(v: &Value) -> Option<(i64, bool)> {
    if let Some(date) = v["date"].as_str() {
        return Some((time::parse_date(date)?, true));
    }
    Some((time::parse_rfc3339(v["dateTime"].as_str()?)?, false))
}

/// Text for the screen: control characters as spaces, the white space collapsed, cut at a
/// character to `max` bytes with an ellipsis.
pub fn text(s: &str, max: usize) -> String {
    let flat = s.split(|c: char| c.is_whitespace() || c.is_control()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    if flat.len() <= max {
        return flat;
    }
    let mut cut = max.saturating_sub('…'.len_utf8());
    while !flat.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", flat[..cut].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> i64 {
        time::parse_rfc3339(s).unwrap()
    }

    #[test]
    fn the_request() {
        let url = events_url(at("2026-11-16T09:30:00Z"));
        let (endpoint, query) = url.split_once('?').unwrap();
        assert_eq!(endpoint, EVENTS_URL);
        let q: std::collections::HashMap<String, String> = form_urlencoded::parse(query.as_bytes()).into_owned().collect();
        assert_eq!(q["timeMin"], "2026-11-16T09:30:00Z");
        assert_eq!(q["timeMax"], "2026-11-23T09:30:00Z");
        assert_eq!((q["singleEvents"].as_str(), q["orderBy"].as_str(), q["maxResults"].as_str()), ("true", "startTime", "40"));
        assert_eq!(q["maxAttendees"], "1");
        assert_eq!(q["fields"], FIELDS);
    }

    // in Google's order (orderBy=startTime, the calendar in Europe/Berlin)
    const LIST: &str = r#"{
      "summary": "anna@example.org",
      "items": [
        {"id": "weekly_20261116T080000Z", "recurringEventId": "weekly", "summary": "Stand-up", "colorId": "7",
         "start": {"dateTime": "2026-11-16T09:00:00+01:00"}, "end": {"dateTime": "2026-11-16T09:15:00+01:00"}},
        {"id": "gone", "status": "cancelled", "summary": "Cancelled", "start": {"dateTime": "2026-11-16T10:00:00Z"}, "end": {"dateTime": "2026-11-16T11:00:00Z"}},
        {"id": "secret", "summary": "Interview at Rival Inc", "location": "Rival HQ", "visibility": "private",
         "start": {"dateTime": "2026-11-16T12:00:00Z"}, "end": {"dateTime": "2026-11-16T13:00:00Z"}},
        {"id": "spam", "summary": "Win a prize", "location": "spam.example", "attendees": [{"self": true, "responseStatus": "declined"}],
         "start": {"dateTime": "2026-11-16T13:00:00Z"}, "end": {"dateTime": "2026-11-16T14:00:00Z"}},
        {"id": "conf", "summary": "Doctor", "visibility": "confidential", "attendees": [{"self": true, "responseStatus": "accepted"}],
         "start": {"dateTime": "2026-11-16T14:00:00Z"}, "end": {"dateTime": "2026-11-16T14:30:00Z"}},
        {"id": "maybe", "summary": "Party", "attendees": [{"self": true, "responseStatus": "needsAction"}, {"email": "b@example.org", "responseStatus": "declined"}],
         "start": {"dateTime": "2026-11-16T19:00:00Z"}, "end": {"dateTime": "2026-11-16T23:00:00Z"}},
        {"id": "bare", "start": {"dateTime": "2026-11-16T20:00:00Z"}},
        {"id": "nostart", "summary": "No start", "end": {"date": "2026-11-16"}},
        {"id": "badstart", "summary": "Bad", "start": {"dateTime": "tomorrow"}},
        {"id": "late", "status": "confirmed", "summary": "Dentist", "location": "Main St 1\nBerlin",
         "colorId": "11", "start": {"dateTime": "2026-11-17T15:00:00+01:00", "timeZone": "Europe/Berlin"},
         "end": {"dateTime": "2026-11-17T16:00:00+01:00"}},
        {"id": "allday", "summary": "Holiday", "start": {"date": "2026-11-18"}, "end": {"date": "2026-11-20"}},
        {"id": "oddcolor", "summary": "Odd colour", "colorId": "12", "start": {"date": "2026-11-21"}}
      ]
    }"#;

    #[test]
    fn events_become_the_agenda() {
        let read = parse_events(LIST).unwrap();
        assert_eq!(read.calendar, "anna@example.org");
        let ids: Vec<&str> = read.events.iter().map(|e| e.id.as_str()).collect();
        // Google's order; cancelled, declined, without a start, with a start that does not parse: left out
        assert_eq!(ids, ["weekly_20261116T080000Z", "secret", "conf", "maybe", "bare", "late", "allday", "oddcolor"]);
        let e = |id: &str| read.events.iter().find(|e| e.id == id).unwrap();

        let timed = e("late");
        assert_eq!((timed.title.as_str(), timed.location.as_str()), ("Dentist", "Main St 1 Berlin"));
        assert_eq!((timed.start, timed.end, timed.all_day), (at("2026-11-17T14:00:00Z"), at("2026-11-17T15:00:00Z"), false));
        assert_eq!(timed.color, 0xdc2127);

        let all_day = e("allday");
        assert!(all_day.all_day);
        assert_eq!((all_day.start, all_day.end), (at("2026-11-18T00:00:00Z"), at("2026-11-20T00:00:00Z")));
        assert_eq!(all_day.color, 0, "no colour of its own");

        for id in ["secret", "conf"] {
            assert_eq!((e(id).title.as_str(), e(id).location.as_str()), (BUSY, ""), "{id}");
        }
        // an invitation not answered yet stays; another guest's answer does not count
        assert_eq!(e("maybe").title, "Party");

        // one occurrence of a recurring event, as Google lists it with singleEvents
        let recurring = e("weekly_20261116T080000Z");
        assert_eq!((recurring.title.as_str(), recurring.start, recurring.color), ("Stand-up", at("2026-11-16T08:00:00Z"), 0x46d6db));

        // missing fields: no title, no place, no end (it ends when it starts)
        let bare = e("bare");
        assert_eq!((bare.title.as_str(), bare.location.as_str(), bare.end), ("", "", bare.start));
        // an all-day event without its end lasts its day; an unknown colour is none
        let odd = e("oddcolor");
        assert_eq!((odd.end - odd.start, odd.color), (DAY, 0));
    }

    #[test]
    fn all_day_events_keep_googles_place() {
        // a calendar in New York (UTC-5): Monday's dinner at 20:00 local, then Tuesday all day.
        // At 00:00 UTC Tuesday's date is 19:00 Monday in New York: sorted by that, it would come first.
        let list = r#"{"items": [
            {"id": "dinner", "start": {"dateTime": "2026-11-16T20:00:00-05:00"}, "end": {"dateTime": "2026-11-16T22:00:00-05:00"}},
            {"id": "tuesday", "start": {"date": "2026-11-17"}, "end": {"date": "2026-11-18"}}
        ]}"#;
        let read = parse_events(list).unwrap();
        assert!(read.events[1].start < read.events[0].start, "the UTC dates would sort the other way");
        assert_eq!(read.events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["dinner", "tuesday"]);
    }

    #[test]
    fn lists_without_events_and_broken_ones() {
        assert_eq!(parse_events(r#"{"summary":"Home"}"#).unwrap(), Read { calendar: "Home".into(), events: vec![] });
        assert_eq!(parse_events("{}").unwrap().events.len(), 0);
        assert!(parse_events(r#"{"items": 3}"#).is_err());
        assert!(parse_events("<html>").is_err());
        // more than the agenda carries: the first forty
        let items: Vec<String> = (0..50).map(|i| format!(r#"{{"id":"e{i}","start":{{"dateTime":"2026-11-16T{:02}:{:02}:00Z"}}}}"#, i / 60, i % 60)).collect();
        let many = parse_events(&format!(r#"{{"items":[{}]}}"#, items.join(","))).unwrap();
        assert_eq!((many.events.len(), many.events[39].id.as_str()), (MAX_EVENTS, "e39"));
    }

    #[test]
    fn api_errors() {
        assert_eq!(api_error(401, br#"{"error":{"code":401,"message":"Request had invalid authentication credentials."}}"#), ApiError::Unauthorized);
        // the Calendar API turned off in the client's project: final until the owner turns it on
        let off = br#"{"error":{"code":403,"message":"Google Calendar API has not been used in project 1 before or it is disabled.",
            "errors":[{"message":"...","domain":"usageLimits","reason":"accessNotConfigured"}],"status":"PERMISSION_DENIED",
            "details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"SERVICE_DISABLED"}]}}"#;
        assert!(matches!(api_error(403, off), ApiError::Disabled(m) if m.contains("has not been used")));
        assert!(matches!(api_error(403, br#"{"error":{"details":[{"reason":"SERVICE_DISABLED"}]}}"#), ApiError::Disabled(_)));
        assert!(matches!(api_error(403, br#"{"error":{"errors":[{"reason":"insufficientPermissions"}]}}"#), ApiError::Scope(_)));
        // a quota is not final
        assert!(matches!(api_error(403, br#"{"error":{"errors":[{"reason":"rateLimitExceeded"}]}}"#), ApiError::Other(_)));
        assert!(matches!(api_error(500, b"<html>"), ApiError::Other(_)));
        assert!(matches!(api_error(404, b""), ApiError::Other(_)));
    }

    #[test]
    fn long_text_is_cut_to_the_catalog() {
        let long = format!(r#"{{"summary":"{}","items":[{{"id":"{}","summary":"{}","location":"{}","start":{{"date":"2026-11-16"}}}}]}}"#, "c".repeat(100), "i".repeat(300), "ä".repeat(300), "l".repeat(300));
        let read = parse_events(&long).unwrap();
        let e = &read.events[0];
        assert!(read.calendar.len() <= MAX_CALENDAR && read.calendar.ends_with('…'));
        assert!(e.id.len() <= MAX_ID && e.title.len() <= MAX_TITLE && e.location.len() <= MAX_LOCATION);
        assert!(e.title.starts_with("ää") && e.title.ends_with('…'));
        assert_eq!(text("a\tb\u{7}c\r\n d ", 64), "a b c d");
    }
}
