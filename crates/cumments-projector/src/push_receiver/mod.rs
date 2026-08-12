//! PushReceiver – AppService push event endpoint.
//!
//! Receives events pushed by the Matrix homeserver via
//! `PUT /_matrix/app/v1/transactions/{txnId}`.
//! and feeds them into the transport-agnostic [`EventProcessor`].
//! The `hs_token` is verified against the configured value before any events
//! are processed. Per the Matrix AppService specification the token arrives

mod auth;
mod parsers;
mod router;
mod state;
mod types;

pub(crate) use parsers::process_single_event;
pub use router::{push_router, push_router_standalone};
pub use state::PushState;
pub(crate) use types::PushEvent;
