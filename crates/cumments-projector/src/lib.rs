//! Event reception for AppService mode.
//!
//! [`event_processor`] contains the transport-agnostic projection core,
//! [`push_receiver`] receives events pushed by the homeserver via HTTP.

pub mod event_processor;
pub mod push_receiver;
