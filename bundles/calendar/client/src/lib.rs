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

//! The calendar client of the calendar bundle (../src/lib.rs): the link to a Google account
//! (OAuth 2.0 for a web application with PKCE, `oauth`), the setup page's requests and answers
//! (`setup`) and its connections (`server`), the reads of a linked account and what each outcome
//! leaves and publishes (`link`), the changes the screen asks for (`change`), Google as a trait
//! (`google`), the panel's address on the network (`net`), the Calendar API's events as the
//! agenda (`events`) and the dates and times it writes (`time`). A crate of its own so that all
//! of it has a host test without SDV; the bundle only loops, sleeps, publishes and passes the
//! screen's changes on.

pub mod change;
pub mod events;
pub mod google;
pub mod link;
pub mod net;
pub mod oauth;
pub mod server;
pub mod setup;
pub mod time;
