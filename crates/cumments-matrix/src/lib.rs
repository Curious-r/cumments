mod wire;

pub mod appservice;
pub mod logging;

pub use appservice::AppServiceMatrixDriver;
pub use logging::LoggingMatrixDriver;

/// Content builders for the Matrix event wire format.
///
/// Exposed publicly (the `wire` module itself stays private) so integration
/// tests can verify that semantic creation relations (`reply_to`,
/// `thread_root`) round-trip through the encoded event and the projector's
/// relation interpretation. They are encoding helpers, not a client API.
pub use wire::{build_location_body, build_media_body, build_message_body, build_poll_body};
