//! The calendar client of the calendar bundle (../src/lib.rs): the link to a Google account
//! (OAuth 2.0 for a web application with PKCE, `oauth`), the setup page's requests and answers
//! (`setup`), the panel's address on the network (`net`), the Calendar API's events as the
//! agenda (`events`) and the dates and times it writes (`time`). A crate of its own so that all
//! of it has a host test without SDV.

pub mod events;
pub mod net;
pub mod oauth;
pub mod setup;
pub mod time;
