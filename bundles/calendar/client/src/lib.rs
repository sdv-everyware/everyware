//! The calendar client of the calendar bundle (../src/lib.rs): the link to a Google account
//! (OAuth 2.0 for a web application with PKCE, `oauth`), the setup page's requests and answers
//! (`setup`) and its connections (`server`), the reads of a linked account and what each outcome
//! leaves and publishes (`link`), Google as a trait (`google`), the panel's address on the network
//! (`net`), the Calendar API's events as the agenda (`events`) and the dates and times it writes
//! (`time`). A crate of its own so that all of it has a host test without SDV; the bundle only
//! loops, sleeps and publishes.

pub mod events;
pub mod google;
pub mod link;
pub mod net;
pub mod oauth;
pub mod server;
pub mod setup;
pub mod time;
