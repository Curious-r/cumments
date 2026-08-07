//! Event reception for AppService mode.
//!
//! [`event_processor`] contains the transport-agnostic projection core,
//! [`push_receiver`] receives events pushed by the homeserver via HTTP,
//! [`backfill`] rebuilds the read model from room history.

pub mod backfill;
pub mod event_processor;
pub mod push_receiver;
