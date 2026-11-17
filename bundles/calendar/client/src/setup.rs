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

//! The panel's setup page on the local network, while no account is linked: what a request
//! asks for and the answers. Two paths, GET only: `/setup?t=<one-time token>` sends the browser
//! to Google's consent screen, `/callback?code=..&state=..` (from the relay page, or from Google
//! itself on a development client) finishes the link. Anything else is refused. The bundle
//! (src/lib.rs) owns the socket: it reads at most `MAX_REQUEST` bytes of a request's head, in
//! time, and closes the connection after the answer.

use std::collections::HashMap;

/// The setup server's port.
pub const PORT: u16 = 8765;
/// A request's head, at most: Google's code and the state are a few hundred bytes.
pub const MAX_REQUEST: usize = 4096;
/// Longest query values taken.
const MAX_TOKEN: usize = 64;
const MAX_CODE: usize = 512;
const MAX_STATE: usize = 512;
const MAX_ERROR: usize = 64;

/// What a request asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// The page of the QR code: start a link, if the token is the one on the screen.
    Setup {
        /// The one-time token of the URL.
        token: String,
    },
    /// Google's answer: the code of the flow of the state's nonce.
    Callback {
        /// The authorization code.
        code: String,
        /// The flow's state (`oauth::State`).
        state: String,
    },
    /// Google's answer without a code: the user said no, or something failed.
    Denied {
        /// Google's error code ("access_denied").
        error: String,
        /// The flow's state, when Google sent it back.
        state: String,
    },
}

/// Why a request is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// Not a request of ours: 400.
    BadRequest,
    /// Another path: 404.
    NotFound,
    /// Another method than GET: 405.
    MethodNotAllowed,
    /// The head is longer than `MAX_REQUEST`: 431.
    TooLarge,
}

/// The length of a request's head with its blank line, once `buf` holds all of it.
pub fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// What a request's head asks for.
pub fn parse(head: &[u8]) -> Result<Request, Reject> {
    if head.len() > MAX_REQUEST {
        return Err(Reject::TooLarge);
    }
    let head = std::str::from_utf8(head).map_err(|_| Reject::BadRequest)?;
    let line = head.split("\r\n").next().unwrap_or("");
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(Reject::BadRequest);
    };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Err(Reject::BadRequest);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/setup" && path != "/callback" {
        return Err(Reject::NotFound);
    }
    if method != "GET" {
        return Err(Reject::MethodNotAllowed);
    }
    // a parameter twice is not ours
    let mut params = HashMap::new();
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        if params.insert(key.into_owned(), value.into_owned()).is_some() {
            return Err(Reject::BadRequest);
        }
    }
    let param = |key: &str, max: usize| params.get(key).filter(|v| !v.is_empty() && v.len() <= max).cloned();
    match path {
        "/setup" => Ok(Request::Setup { token: param("t", MAX_TOKEN).ok_or(Reject::BadRequest)? }),
        _ => match params.get("error") {
            Some(_) => Ok(Request::Denied {
                error: param("error", MAX_ERROR).filter(|e| e.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')).ok_or(Reject::BadRequest)?,
                state: param("state", MAX_STATE).unwrap_or_default(),
            }),
            None => Ok(Request::Callback { code: param("code", MAX_CODE).ok_or(Reject::BadRequest)?, state: param("state", MAX_STATE).ok_or(Reject::BadRequest)? }),
        },
    }
}

/// 302 to `location` (Google's consent screen).
pub fn redirect(location: &str) -> Vec<u8> {
    response("302 Found", &[("Location", location)], "")
}

/// A page with one sentence: `status` ("200 OK") and `message` (escaped).
pub fn page(status: &str, message: &str) -> Vec<u8> {
    let body = format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Calendar link</title><p>{}</p></html>\n",
        escape(message)
    );
    response(status, &[("Content-Type", "text/html; charset=utf-8")], &body)
}

/// The answer to a refused request.
pub fn reject(why: Reject) -> Vec<u8> {
    match why {
        Reject::BadRequest => page("400 Bad Request", "This is not a link of this panel."),
        Reject::NotFound => page("404 Not Found", "Not found."),
        Reject::MethodNotAllowed => page("405 Method Not Allowed", "Not allowed."),
        Reject::TooLarge => page("431 Request Header Fields Too Large", "The request is too long."),
    }
}

fn response(status: &str, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    // nothing cached, no referrer to Google or anywhere, nothing loaded or run by the page
    head.push_str(&format!(
        "Content-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body.as_bytes());
    bytes
}

/// Text as HTML.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(target: &str) -> Result<Request, Reject> {
        parse(format!("GET {target} HTTP/1.1\r\nHost: 192.168.1.20:8765\r\nUser-Agent: phone\r\n\r\n").as_bytes())
    }

    #[test]
    fn good_requests() {
        assert_eq!(get("/setup?t=abc_-1"), Ok(Request::Setup { token: "abc_-1".into() }));
        assert_eq!(
            get("/callback?state=eyJwIjoiMTkyIn0&code=4%2F0AbCd-e&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcalendar.events.readonly"),
            Ok(Request::Callback { code: "4/0AbCd-e".into(), state: "eyJwIjoiMTkyIn0".into() })
        );
        assert_eq!(get("/callback?error=access_denied&state=s"), Ok(Request::Denied { error: "access_denied".into(), state: "s".into() }));
        assert_eq!(get("/callback?error=access_denied"), Ok(Request::Denied { error: "access_denied".into(), state: String::new() }));
        // HTTP/1.0 too
        assert_eq!(parse(b"GET /setup?t=x HTTP/1.0\r\n\r\n"), Ok(Request::Setup { token: "x".into() }));
    }

    #[test]
    fn other_paths_and_methods() {
        for target in ["/", "/favicon.ico", "/setup/", "/SETUP?t=x", "/callback/../setup?t=x"] {
            assert_eq!(get(target), Err(Reject::NotFound), "{target}");
        }
        assert_eq!(parse(b"POST /callback?code=a&state=b HTTP/1.1\r\n\r\n"), Err(Reject::MethodNotAllowed));
        assert_eq!(parse(b"HEAD /setup?t=x HTTP/1.1\r\n\r\n"), Err(Reject::MethodNotAllowed));
        assert_eq!(parse(b"OPTIONS * HTTP/1.1\r\n\r\n"), Err(Reject::BadRequest));
        assert_eq!(parse(b"GET http://evil/setup?t=x HTTP/1.1\r\n\r\n"), Err(Reject::BadRequest));
    }

    #[test]
    fn bad_requests() {
        for target in [
            "/setup",
            "/setup?t=",
            "/setup?x=1",
            "/setup?t=a&t=b",
            "/callback",
            "/callback?code=a",
            "/callback?state=b",
            "/callback?code=&state=b",
            "/callback?error=%3Cscript%3E&state=s",
        ] {
            assert_eq!(get(target), Err(Reject::BadRequest), "{target}");
        }
        assert_eq!(get(&format!("/setup?t={}", "a".repeat(MAX_TOKEN + 1))), Err(Reject::BadRequest));
        assert_eq!(get(&format!("/callback?code={}&state=s", "a".repeat(MAX_CODE + 1))), Err(Reject::BadRequest));
        assert_eq!(parse(b"GET /setup?t=x\r\n\r\n"), Err(Reject::BadRequest));
        assert_eq!(parse(b"GET /setup?t=x HTTP/1.1 extra\r\n\r\n"), Err(Reject::BadRequest));
        assert_eq!(parse(b"GET /setup?t=x SSH-2.0\r\n\r\n"), Err(Reject::BadRequest));
        assert_eq!(parse(b"\xff\xfe\r\n\r\n"), Err(Reject::BadRequest));
        assert_eq!(parse(b""), Err(Reject::BadRequest));
    }

    #[test]
    fn too_long() {
        let long = format!("GET /setup?t=x HTTP/1.1\r\nCookie: {}\r\n\r\n", "a".repeat(MAX_REQUEST));
        assert_eq!(parse(long.as_bytes()), Err(Reject::TooLarge));
    }

    #[test]
    fn the_head_ends_with_a_blank_line() {
        assert_eq!(head_end(b"GET / HTTP/1.1\r\nHost: x\r\n"), None);
        assert_eq!(head_end(b"GET / HTTP/1.1\r\n\r\nrest"), Some(18));
    }

    #[test]
    fn answers() {
        let page = String::from_utf8(page("200 OK", "Done. <You> can close this page.")).unwrap();
        assert!(page.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(page.contains("Content-Security-Policy: default-src 'none'\r\n") && page.contains("Cache-Control: no-store\r\n"));
        assert!(page.contains("<p>Done. &lt;You&gt; can close this page.</p>"));
        let (head, body) = page.split_once("\r\n\r\n").unwrap();
        assert!(head.contains(&format!("Content-Length: {}\r\n", body.len())));
        let redirect = String::from_utf8(redirect("https://accounts.google.com/o/oauth2/v2/auth?a=b")).unwrap();
        assert!(redirect.starts_with("HTTP/1.1 302 Found\r\nLocation: https://accounts.google.com/o/oauth2/v2/auth?a=b\r\n"));
        assert!(redirect.ends_with("Content-Length: 0\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'\r\nConnection: close\r\n\r\n"));
        assert!(String::from_utf8(reject(Reject::TooLarge)).unwrap().starts_with("HTTP/1.1 431 "));
    }
}
