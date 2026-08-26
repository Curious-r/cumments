//! Event reception for AppService mode.
//!
//! [`event_processor`] contains the transport-agnostic projection core,
//! [`parsed`] the wire-agnostic event structures, [`verification`] the
//! Cumments claim verifiers, [`push_receiver`] receives events pushed by the
//! homeserver via HTTP and [`backfill`] rebuilds the read model from history.

pub mod backfill;
pub mod bot_commands;
pub mod ephemeral;
pub mod event_processor;
pub mod parsed;
pub mod push_receiver;
pub mod sse_outbox;
pub mod verification;
