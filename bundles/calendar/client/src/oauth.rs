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

//! Google's OAuth 2.0 for a web application, as the panel links an account: the client of the
//! panel's owner (`ClientConfig`), the access the owner chooses (`Access`), the authorization
//! request with PKCE (RFC 7636, S256) and a one-time state, the setup page's one-time token, and
//! the token endpoint (the code exchanged, the refresh token used, the scope Google granted).
//! <https://developers.google.com/identity/protocols/oauth2/web-server>

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

/// Google's authorization endpoint (the consent screen).
pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Google's token endpoint.
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
/// The scope of reading the events.
pub const READ_SCOPE: &str = "https://www.googleapis.com/auth/calendar.events.readonly";
/// The scope of reading and changing the events: the answers to invitations, the deletions.
pub const CHANGE_SCOPE: &str = "https://www.googleapis.com/auth/calendar.events";
/// A flow begun on the setup page can be finished for this long.
pub const FLOW_TTL: Duration = Duration::from_secs(600);
/// The setup page's token is replaced this often (and after every use).
pub const SETUP_ROTATION: Duration = Duration::from_secs(600);
/// A replaced token is still good for this long: the QR code may have been scanned just before.
const SETUP_GRACE: Duration = Duration::from_secs(60);
/// Flows begun and not finished, at most: the oldest goes first.
const MAX_FLOWS: usize = 16;
/// An answer of the token endpoint is a few kilobytes.
const MAX_TOKEN_REPLY: usize = 64 * 1024;
/// A state longer than this is not one of ours.
const MAX_STATE: usize = 512;

/// What the panel may do with the account's events: the owner's choice on the setup page, and
/// what Google granted (the owner may untick a scope on the consent screen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Read the events (`READ_SCOPE`).
    Read,
    /// Read and change them (`CHANGE_SCOPE`).
    Change,
}

impl Access {
    /// The scopes the authorization request asks for. "Read and change" asks for both, so a
    /// consent that unticks the change still links the calendar, for reading.
    pub fn scopes(self) -> String {
        match self {
            Access::Read => READ_SCOPE.to_string(),
            Access::Change => format!("{READ_SCOPE} {CHANGE_SCOPE}"),
        }
    }

    /// What `scope` (Google's list of granted scopes, separated by spaces) allows: Change with
    /// `CHANGE_SCOPE`, Read with `READ_SCOPE` alone, None with neither.
    pub fn granted(scope: &str) -> Option<Access> {
        let has = |wanted: &str| scope.split(' ').any(|s| s == wanted);
        if has(CHANGE_SCOPE) {
            Some(Access::Change)
        } else if has(READ_SCOPE) {
            Some(Access::Read)
        } else {
            None
        }
    }
}

/// The OAuth client of the panel's owner, from Google's downloaded client JSON (a client of the
/// type "Web application": `{"web": {"client_id", "client_secret", "redirect_uris", ...}}`).
#[derive(Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// The client's id.
    pub client_id: String,
    /// The client's secret (Google's web clients have one; with PKCE it is not the only proof).
    pub client_secret: String,
    /// Where Google sends the browser back: the first of the client's redirect URIs, the relay
    /// page (https) or, for development, `http://localhost:8765/callback`.
    pub redirect_uri: String,
}

impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig").field("client_id", &self.client_id).field("redirect_uri", &self.redirect_uri).finish_non_exhaustive()
    }
}

impl ClientConfig {
    /// The client of Google's client JSON, or why it is not usable.
    pub fn parse(json: &str) -> Result<ClientConfig, String> {
        let v: Value = serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
        let web = v.get("web").ok_or("no \"web\" client (Google Cloud console: an OAuth client of the type Web application)")?;
        let text = |key: &str| web.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string).ok_or_else(|| format!("no {key}"));
        let client_id = text("client_id")?;
        let client_secret = text("client_secret")?;
        let redirect_uri = web.get("redirect_uris").and_then(|r| r.get(0)).and_then(Value::as_str).ok_or("no redirect_uris")?.to_string();
        // Google takes https, and plain http only for localhost
        if !(redirect_uri.starts_with("https://") || redirect_uri.starts_with("http://localhost:") || redirect_uri.starts_with("http://localhost/")) {
            return Err(format!("redirect URI {redirect_uri:?}: neither https nor http://localhost"));
        }
        Ok(ClientConfig { client_id, client_secret, redirect_uri })
    }
}

/// `n` random bytes as base64url without padding: a token, a nonce, a PKCE verifier.
pub fn random_token(n: usize) -> String {
    let mut bytes = vec![0; n];
    // BoringSSL's RAND_bytes does not fail (it aborts the process instead)
    openssl::rand::rand_bytes(&mut bytes).expect("random bytes");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The proof key of one flow (RFC 7636): the verifier stays on the panel, the challenge goes to
/// Google with the authorization request.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    /// 43 characters of base64url (32 random bytes).
    pub verifier: String,
    /// base64url(SHA-256(verifier)), the method S256.
    pub challenge: String,
}

impl Pkce {
    /// A new verifier and its challenge.
    pub fn generate() -> Pkce {
        Pkce::of(random_token(32))
    }

    /// The challenge of `verifier`.
    pub fn of(verifier: String) -> Pkce {
        let challenge = URL_SAFE_NO_PAD.encode(openssl::sha::sha256(verifier.as_bytes()));
        Pkce { verifier, challenge }
    }
}

/// The OAuth state of a flow: the panel's address for the relay page (`p`, "192.168.1.20:8765")
/// and the flow's one-time nonce (`n`), as base64url of `{"p": ..., "n": ...}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    /// host:port of the panel's setup server, where the relay page sends the browser back.
    pub panel: String,
    /// The flow's nonce (`Flows`).
    pub nonce: String,
}

impl State {
    /// The state as it goes into the authorization request.
    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::json!({ "p": self.panel, "n": self.nonce }).to_string())
    }

    /// A state of ours, or None.
    pub fn decode(state: &str) -> Option<State> {
        if state.len() > MAX_STATE {
            return None;
        }
        let json = URL_SAFE_NO_PAD.decode(state.trim_end_matches('=')).ok()?;
        let v: Value = serde_json::from_slice(&json).ok()?;
        Some(State { panel: v.get("p")?.as_str()?.to_string(), nonce: v.get("n")?.as_str()?.to_string() })
    }
}

/// The authorization request: the consent screen for the scopes of `access`, a refresh token
/// (offline, consent asked every time so that Google sends one), PKCE S256 and the state.
pub fn auth_url(client: &ClientConfig, access: Access, challenge: &str, state: &str) -> String {
    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &client.client_id)
        .append_pair("redirect_uri", &client.redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &access.scopes())
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .finish();
    format!("{AUTH_ENDPOINT}?{query}")
}

/// The flows begun on the setup page and not finished: per nonce the PKCE verifier and when it
/// began. A nonce is good once, for `FLOW_TTL`.
#[derive(Default)]
pub struct Flows {
    flows: HashMap<String, (String, Instant)>,
}

impl Flows {
    /// A new flow: its nonce and its proof key.
    pub fn begin(&mut self, now: Instant) -> (String, Pkce) {
        self.flows.retain(|_, (_, began)| now.saturating_duration_since(*began) <= FLOW_TTL);
        while self.flows.len() >= MAX_FLOWS {
            let Some(oldest) = self.flows.iter().min_by_key(|(_, (_, began))| *began).map(|(n, _)| n.clone()) else { break };
            self.flows.remove(&oldest);
        }
        let nonce = random_token(16);
        let pkce = Pkce::generate();
        self.flows.insert(nonce.clone(), (pkce.verifier.clone(), now));
        (nonce, pkce)
    }

    /// The verifier of the flow of `nonce`, which ends here; None for a nonce that is not one of
    /// the flows, was used, or is older than `FLOW_TTL`.
    pub fn take(&mut self, nonce: &str, now: Instant) -> Option<String> {
        let (verifier, began) = self.flows.remove(nonce)?;
        (now.saturating_duration_since(began) <= FLOW_TTL).then_some(verifier)
    }
}

/// The one-time token of the setup page's URL (the QR code on the screen): a new one after
/// every use and every `SETUP_ROTATION`; the one it replaced by time stays good for a minute.
pub struct SetupToken {
    current: (String, Instant),
    previous: Option<(String, Instant)>,
}

impl SetupToken {
    /// A first token.
    pub fn new(now: Instant) -> SetupToken {
        SetupToken { current: (random_token(32), now), previous: None }
    }

    /// The token the QR code carries now.
    pub fn value(&self) -> &str {
        &self.current.0
    }

    /// A new token when the current one is `SETUP_ROTATION` old; true when it was replaced.
    pub fn rotate_if_due(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.current.1) < SETUP_ROTATION {
            return false;
        }
        let old = std::mem::replace(&mut self.current, (random_token(32), now));
        self.previous = Some((old.0, now));
        true
    }

    /// Takes `presented` when it is the current token or the replaced one within its minute; a
    /// token taken is good no more, and the current one is replaced (the screen shows a new QR).
    pub fn take(&mut self, presented: &str, now: Instant) -> bool {
        if same(presented, &self.current.0) {
            self.current = (random_token(32), now);
            return true;
        }
        let previous = self.previous.as_ref().is_some_and(|(token, replaced)| same(presented, token) && now.saturating_duration_since(*replaced) <= SETUP_GRACE);
        if previous {
            self.previous = None;
        }
        previous
    }
}

/// Equal in a time that does not tell how much of a secret matched.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && openssl::memcmp::eq(a.as_bytes(), b.as_bytes())
}

/// What the token endpoint gave.
#[derive(Clone, PartialEq, Eq)]
pub struct Tokens {
    /// The access token for the Calendar API.
    pub access_token: String,
    /// Seconds the access token is good for.
    pub expires_in: u64,
    /// The refresh token: after a code exchange (access_type=offline); a refresh may bring a new one.
    pub refresh_token: Option<String>,
    /// The scopes Google granted, as it wrote them, when the answer says (an exchange does, a
    /// refresh usually too): a calendar scope at least (`Access::granted`).
    pub scope: Option<String>,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_in", &self.expires_in)
            .field("refresh_token", &self.refresh_token.is_some())
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// Why the token endpoint gave no tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// `invalid_grant`: the code or the refresh token is not good (revoked, expired, used): the
    /// account has to be linked again.
    Grant(String),
    /// The user granted neither calendar scope (Google's consent screen lets one untick them).
    Scope,
    /// Google refused the OAuth client (`invalid_client`, `unauthorized_client`, `deleted_client`:
    /// the client was deleted, or its secret was reset): it stays refused, a new client file is
    /// needed.
    Client(String),
    /// Anything else (the network, Google): try again later.
    Other(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::Grant(why) => write!(f, "the grant is not good ({why})"),
            AuthError::Scope => write!(f, "the calendar permission was not granted"),
            AuthError::Client(why) => write!(f, "Google refused the OAuth client ({why})"),
            AuthError::Other(why) => f.write_str(why),
        }
    }
}

/// The token endpoint's answer: its tokens or its error (`{"error": ..., "error_description": ...}`).
pub fn parse_token_response(status: u16, body: &str) -> Result<Tokens, AuthError> {
    let v: Value = serde_json::from_str(body).map_err(|_| AuthError::Other(format!("token endpoint: HTTP {status}, not JSON")))?;
    if let Some(error) = v.get("error").and_then(Value::as_str) {
        let description = v.get("error_description").and_then(Value::as_str).unwrap_or("");
        let why = if description.is_empty() { error.to_string() } else { format!("{error}: {description}") };
        return Err(match error {
            "invalid_grant" => AuthError::Grant(why),
            "invalid_client" | "unauthorized_client" | "deleted_client" => AuthError::Client(why),
            _ => AuthError::Other(why),
        });
    }
    if !(200..300).contains(&status) {
        return Err(AuthError::Other(format!("token endpoint: HTTP {status}")));
    }
    let access_token = v.get("access_token").and_then(Value::as_str).filter(|t| !t.is_empty()).ok_or_else(|| AuthError::Other("no access_token".into()))?;
    let scope = v.get("scope").and_then(Value::as_str);
    if scope.is_some_and(|scope| Access::granted(scope).is_none()) {
        return Err(AuthError::Scope);
    }
    Ok(Tokens {
        access_token: access_token.to_string(),
        expires_in: v.get("expires_in").and_then(Value::as_u64).unwrap_or(0),
        refresh_token: v.get("refresh_token").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string),
        scope: scope.map(str::to_string),
    })
}

/// The form that exchanges the code of a flow for tokens.
pub fn exchange_form(client: &ClientConfig, code: &str, verifier: &str) -> String {
    form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair("code_verifier", verifier)
        .append_pair("client_id", &client.client_id)
        .append_pair("client_secret", &client.client_secret)
        .append_pair("redirect_uri", &client.redirect_uri)
        .finish()
}

/// The form that gets a new access token with the refresh token.
pub fn refresh_form(client: &ClientConfig, refresh_token: &str) -> String {
    form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", refresh_token)
        .append_pair("client_id", &client.client_id)
        .append_pair("client_secret", &client.client_secret)
        .finish()
}

/// The code of a flow exchanged for tokens.
pub async fn exchange(client: &ClientConfig, code: &str, verifier: &str) -> Result<Tokens, AuthError> {
    token_request(&exchange_form(client, code, verifier)).await
}

/// A new access token with the refresh token.
pub async fn refresh(client: &ClientConfig, refresh_token: &str) -> Result<Tokens, AuthError> {
    token_request(&refresh_form(client, refresh_token)).await
}

async fn token_request(form: &str) -> Result<Tokens, AuthError> {
    let reply = everyware_https::post_form(TOKEN_ENDPOINT, form, MAX_TOKEN_REPLY).await.map_err(|e| AuthError::Other(e.to_string()))?;
    parse_token_response(reply.status, &String::from_utf8_lossy(&reply.body))
}

/// What the panel keeps of a linked account: the refresh token, the client it belongs to (a new
/// client makes the grant worthless) and the scopes Google granted.
#[derive(Clone, PartialEq, Eq)]
pub struct Grant {
    /// The client the refresh token was issued to.
    pub client_id: String,
    /// The refresh token.
    pub refresh_token: String,
    /// The scopes Google granted, as it wrote them; empty in a grant kept before the panel asked
    /// for a choice (read access, the only one then).
    pub scope: String,
}

impl fmt::Debug for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Grant").field("client_id", &self.client_id).field("scope", &self.scope).finish_non_exhaustive()
    }
}

impl Grant {
    /// What the grant allows: read access when its scope says no more (or nothing).
    pub fn access(&self) -> Access {
        Access::granted(&self.scope).unwrap_or(Access::Read)
    }

    /// The grant as it is kept (JSON).
    pub fn to_json(&self) -> String {
        serde_json::json!({ "client_id": self.client_id, "refresh_token": self.refresh_token, "scope": self.scope }).to_string()
    }

    /// A kept grant.
    pub fn parse(json: &[u8]) -> Option<Grant> {
        let v: Value = serde_json::from_slice(json).ok()?;
        let text = |key: &str| v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
        Some(Grant { client_id: text("client_id")?, refresh_token: text("refresh_token")?, scope: text("scope").unwrap_or_default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> ClientConfig {
        ClientConfig::parse(
            r#"{"web":{"client_id":"id-1.apps.googleusercontent.com","project_id":"p","auth_uri":"https://accounts.google.com/o/oauth2/auth",
               "client_secret":"s3cret","redirect_uris":["https://relay.example.org/oauth/","http://localhost:8765/callback"]}}"#,
        )
        .unwrap()
    }

    #[test]
    fn the_client_json() {
        let c = client();
        assert_eq!((c.client_id.as_str(), c.client_secret.as_str(), c.redirect_uri.as_str()), ("id-1.apps.googleusercontent.com", "s3cret", "https://relay.example.org/oauth/"));
        assert!(!format!("{c:?}").contains("s3cret"));
        let dev = ClientConfig::parse(r#"{"web":{"client_id":"a","client_secret":"b","redirect_uris":["http://localhost:8765/callback"]}}"#).unwrap();
        assert_eq!(dev.redirect_uri, "http://localhost:8765/callback");
        for bad in [
            "",
            "{}",
            r#"{"installed":{"client_id":"a","client_secret":"b","redirect_uris":["http://localhost"]}}"#,
            r#"{"web":{"client_id":"a","redirect_uris":["https://x/"]}}"#,
            r#"{"web":{"client_id":"a","client_secret":"b","redirect_uris":[]}}"#,
            r#"{"web":{"client_id":"a","client_secret":"b","redirect_uris":["http://192.168.1.2:8765/callback"]}}"#,
        ] {
            assert!(ClientConfig::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn pkce_s256_of_rfc_7636() {
        // RFC 7636, appendix B
        let p = Pkce::of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let g = Pkce::generate();
        assert_eq!(g.verifier.len(), 43);
        assert!(g.verifier.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
        assert_eq!(Pkce::of(g.verifier.clone()).challenge, g.challenge);
        assert_ne!(Pkce::generate().verifier, g.verifier);
    }

    #[test]
    fn the_authorization_request() {
        let url = auth_url(&client(), Access::Read, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "c3RhdGU");
        let (endpoint, query) = url.split_once('?').unwrap();
        assert_eq!(endpoint, AUTH_ENDPOINT);
        let q: HashMap<String, String> = form_urlencoded::parse(query.as_bytes()).into_owned().collect();
        assert_eq!(q["client_id"], "id-1.apps.googleusercontent.com");
        assert_eq!(q["redirect_uri"], "https://relay.example.org/oauth/");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["scope"], READ_SCOPE);
        assert_eq!(q["access_type"], "offline");
        assert_eq!(q["prompt"], "consent");
        assert_eq!(q["code_challenge"], "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], "c3RhdGU");
        assert!(!q.contains_key("client_secret"));
        assert_eq!(q.len(), 9);
        // read and change: both scopes, so that unticking the change still links for reading
        let url = auth_url(&client(), Access::Change, "c", "s");
        let q: HashMap<String, String> = form_urlencoded::parse(url.split_once('?').unwrap().1.as_bytes()).into_owned().collect();
        assert_eq!(q["scope"], format!("{READ_SCOPE} {CHANGE_SCOPE}"));
    }

    #[test]
    fn the_access_google_granted() {
        assert_eq!(Access::granted(READ_SCOPE), Some(Access::Read));
        assert_eq!(Access::granted(CHANGE_SCOPE), Some(Access::Change));
        assert_eq!(Access::granted(&format!("{READ_SCOPE} {CHANGE_SCOPE}")), Some(Access::Change));
        assert_eq!(Access::granted(&format!("openid {CHANGE_SCOPE} email")), Some(Access::Change));
        assert_eq!(Access::granted("openid email"), None);
        assert_eq!(Access::granted(""), None);
        // a scope that only begins like ours is not ours
        assert_eq!(Access::granted(&format!("{CHANGE_SCOPE}.owned {READ_SCOPE}x")), None);
        assert_eq!(Access::granted("https://www.googleapis.com/auth/calendar"), None);
    }

    #[test]
    fn the_state_goes_there_and_back() {
        let s = State { panel: "192.168.1.20:8765".into(), nonce: "abc-_1".into() };
        let encoded = s.encode();
        assert!(encoded.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'), "{encoded}");
        assert_eq!(State::decode(&encoded), Some(s.clone()));
        // what the relay page's decoder reads: base64url of the JSON
        let json = String::from_utf8(URL_SAFE_NO_PAD.decode(&encoded).unwrap()).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&json).unwrap(), serde_json::json!({"p": "192.168.1.20:8765", "n": "abc-_1"}));
        assert_eq!(State::decode(&format!("{encoded}==")), Some(s));
        assert_eq!(State::decode("not base64!"), None);
        assert_eq!(State::decode(&URL_SAFE_NO_PAD.encode(r#"{"p":"x"}"#)), None);
        assert_eq!(State::decode(&URL_SAFE_NO_PAD.encode(r#"{"p":1,"n":"x"}"#)), None);
        assert_eq!(State::decode(&"A".repeat(MAX_STATE + 1)), None);
    }

    #[test]
    fn a_nonce_is_good_once_and_for_ten_minutes() {
        let t0 = Instant::now();
        let mut flows = Flows::default();
        let (n1, p1) = flows.begin(t0);
        let (n2, _) = flows.begin(t0);
        assert_ne!(n1, n2);
        assert_eq!(flows.take(&n1, t0 + Duration::from_secs(60)), Some(p1.verifier));
        assert_eq!(flows.take(&n1, t0 + Duration::from_secs(61)), None, "used");
        assert_eq!(flows.take(&n2, t0 + FLOW_TTL + Duration::from_secs(1)), None, "expired");
        assert_eq!(flows.take("unknown", t0), None);
        // the flows are bounded: the oldest goes
        let (first, _) = flows.begin(t0);
        for i in 0..MAX_FLOWS {
            flows.begin(t0 + Duration::from_secs(1 + i as u64));
        }
        assert_eq!(flows.flows.len(), MAX_FLOWS);
        assert_eq!(flows.take(&first, t0 + Duration::from_secs(30)), None);
    }

    #[test]
    fn the_setup_token_is_one_time_and_rotates() {
        let t0 = Instant::now();
        let mut token = SetupToken::new(t0);
        let first = token.value().to_string();
        assert_eq!(first.len(), 43);
        assert!(!token.take("wrong", t0));
        assert!(!token.take(&first[..42], t0));
        assert!(token.take(&first, t0));
        assert_ne!(token.value(), first, "a new token after a use");
        assert!(!token.take(&first, t0), "used");
        // every ten minutes a new one; the replaced one is good for a minute more
        let second = token.value().to_string();
        assert!(!token.rotate_if_due(t0 + Duration::from_secs(599)));
        assert!(token.rotate_if_due(t0 + SETUP_ROTATION));
        assert!(token.take(&second, t0 + SETUP_ROTATION + Duration::from_secs(30)));
        assert!(!token.take(&second, t0 + SETUP_ROTATION + Duration::from_secs(31)), "used");
        let third = token.value().to_string();
        let t1 = t0 + SETUP_ROTATION * 2;
        assert!(token.rotate_if_due(t1));
        assert!(!token.take(&third, t1 + SETUP_GRACE + Duration::from_secs(1)), "replaced too long ago");
    }

    #[test]
    fn token_answers_and_errors() {
        let ok = parse_token_response(200, &format!(r#"{{"access_token":"ya29.a","expires_in":3599,"refresh_token":"1//r","scope":"{READ_SCOPE}","token_type":"Bearer"}}"#)).unwrap();
        assert_eq!((ok.access_token.as_str(), ok.expires_in, ok.refresh_token.as_deref()), ("ya29.a", 3599, Some("1//r")));
        assert_eq!(ok.scope.as_deref(), Some(READ_SCOPE));
        assert!(!format!("{ok:?}").contains("ya29") && !format!("{ok:?}").contains("1//r"));
        // the change granted, in Google's order
        let change = parse_token_response(200, &format!(r#"{{"access_token":"a","expires_in":1,"scope":"{CHANGE_SCOPE} {READ_SCOPE}"}}"#)).unwrap();
        assert_eq!(change.scope.as_deref().and_then(Access::granted), Some(Access::Change));
        // a refresh: no new refresh token, no scope
        let refreshed = parse_token_response(200, r#"{"access_token":"ya29.b","expires_in":3599,"token_type":"Bearer"}"#).unwrap();
        assert_eq!((refreshed.refresh_token, refreshed.scope), (None, None));
        assert_eq!(
            parse_token_response(400, r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#),
            Err(AuthError::Grant("invalid_grant: Token has been expired or revoked.".into()))
        );
        // the client deleted or its secret reset: refused until a new client file
        assert_eq!(parse_token_response(401, r#"{"error":"invalid_client","error_description":"Unauthorized"}"#), Err(AuthError::Client("invalid_client: Unauthorized".into())));
        assert!(matches!(parse_token_response(400, r#"{"error":"unauthorized_client"}"#), Err(AuthError::Client(_))));
        assert!(matches!(parse_token_response(401, r#"{"error":"deleted_client","error_description":"The OAuth client was deleted."}"#), Err(AuthError::Client(_))));
        assert!(matches!(parse_token_response(400, r#"{"error":"invalid_request"}"#), Err(AuthError::Other(_))));
        assert_eq!(parse_token_response(200, r#"{"access_token":"a","expires_in":1,"scope":"openid email"}"#), Err(AuthError::Scope));
        assert!(matches!(parse_token_response(502, "<html>Bad Gateway</html>"), Err(AuthError::Other(_))));
        assert!(matches!(parse_token_response(500, "{}"), Err(AuthError::Other(_))));
        assert!(matches!(parse_token_response(200, r#"{"expires_in":1}"#), Err(AuthError::Other(_))));
    }

    #[test]
    fn the_token_forms() {
        let c = client();
        let form: HashMap<String, String> = form_urlencoded::parse(exchange_form(&c, "4/0Ab+c d", "v").as_bytes()).into_owned().collect();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code"], "4/0Ab+c d");
        assert_eq!(form["code_verifier"], "v");
        assert_eq!(form["redirect_uri"], "https://relay.example.org/oauth/");
        assert_eq!(form["client_secret"], "s3cret");
        let form: HashMap<String, String> = form_urlencoded::parse(refresh_form(&c, "1//r").as_bytes()).into_owned().collect();
        assert_eq!((form["grant_type"].as_str(), form["refresh_token"].as_str(), form["client_id"].as_str()), ("refresh_token", "1//r", "id-1.apps.googleusercontent.com"));
    }

    #[test]
    fn a_grant_is_kept_as_json() {
        let g = Grant { client_id: "id".into(), refresh_token: "1//r".into(), scope: format!("{READ_SCOPE} {CHANGE_SCOPE}") };
        assert_eq!(Grant::parse(g.to_json().as_bytes()), Some(g.clone()));
        assert_eq!(g.access(), Access::Change);
        assert!(!format!("{g:?}").contains("1//r"));
        assert_eq!(Grant::parse(br#"{"client_id":"id"}"#), None);
        assert_eq!(Grant::parse(b"garbage"), None);
        // a grant kept before the choice: read access
        let old = Grant::parse(br#"{"client_id":"id","refresh_token":"1//r"}"#).unwrap();
        assert_eq!((old.scope.as_str(), old.access()), ("", Access::Read));
        assert_eq!(Grant { scope: READ_SCOPE.into(), ..g }.access(), Access::Read);
    }
}
