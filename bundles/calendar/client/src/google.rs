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

//! What the calendar asks Google, as a trait: the bundle uses `Live` (HTTPS to Google), the
//! tests a fake that answers from a script.

use crate::change::{self, ApiCall};
use crate::events::{self, ApiError, Read};
use crate::oauth::{self, AuthError, ClientConfig, Tokens};
use everyware_https::Reply;

/// Google's token endpoint and Calendar API.
#[async_trait::async_trait]
pub trait Google: Send + Sync {
    /// The code of a flow exchanged for tokens.
    async fn exchange(&self, client: &ClientConfig, code: &str, verifier: &str) -> Result<Tokens, AuthError>;
    /// A new access token with a refresh token.
    async fn refresh(&self, client: &ClientConfig, refresh_token: &str) -> Result<Tokens, AuthError>;
    /// The grant of a refresh or access token ended (`oauth::parse_revoke_response`).
    async fn revoke(&self, token: &str) -> Result<(), AuthError>;
    /// The primary calendar's events from `now` (unix time) on, with an access token.
    async fn events(&self, access_token: &str, now: i64) -> Result<Read, ApiError>;
    /// One call of a change (change.rs), with an access token: Google's answer whatever its
    /// status, or why there is none (the network).
    async fn call(&self, access_token: &str, call: &ApiCall) -> Result<Reply, String>;
}

/// Google itself, over HTTPS (libs/https).
pub struct Live;

#[async_trait::async_trait]
impl Google for Live {
    async fn exchange(&self, client: &ClientConfig, code: &str, verifier: &str) -> Result<Tokens, AuthError> {
        oauth::exchange(client, code, verifier).await
    }

    async fn refresh(&self, client: &ClientConfig, refresh_token: &str) -> Result<Tokens, AuthError> {
        oauth::refresh(client, refresh_token).await
    }

    async fn revoke(&self, token: &str) -> Result<(), AuthError> {
        oauth::revoke(token).await
    }

    async fn events(&self, access_token: &str, now: i64) -> Result<Read, ApiError> {
        events::read(access_token, now).await
    }

    async fn call(&self, access_token: &str, call: &ApiCall) -> Result<Reply, String> {
        everyware_https::bearer(call.method, &call.url, access_token, call.if_match.as_deref(), call.body.as_deref(), change::MAX_REPLY).await.map_err(|e| e.to_string())
    }
}
