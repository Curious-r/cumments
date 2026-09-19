//! The Matrix complete-event size limit.
//!
//! Matrix bounds the size of a room event. That bound is a protocol
//! constraint: it applies to the complete event, not to any single Cumments
//! semantic field, and the homeserver is the final authority for whether a
//! given write is accepted. Cumments sends Client-Server API content and does
//! not construct the federation event locally, so nothing here approximates
//! that event or preflights an API request against it.

/// Maximum size, in bytes, of a complete Matrix room event.
pub const MAX_COMPLETE_EVENT_BYTES: usize = 65_536;

/// Whether an already-encoded complete Matrix event fits the protocol limit.
///
/// The input must be the complete event representation — the JSON the
/// homeserver treats as the event, including every field the Matrix size rule
/// covers — and it must already be canonical-JSON encoded, so that the byte
/// length measured here is the length that matters.
///
/// This is not a rule about the size of Client-Server API request content.
/// Where Cumments only holds API content it cannot know the size of the event
/// the homeserver will construct, so such code must let the homeserver decide
/// and handle its `M_TOO_LARGE` response instead of preflighting.
pub fn complete_event_fits(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_COMPLETE_EVENT_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_is_the_matrix_event_size_limit() {
        assert_eq!(MAX_COMPLETE_EVENT_BYTES, 65_536);
    }

    #[test]
    fn checks_the_boundary_exactly() {
        assert!(complete_event_fits(&vec![b'x'; 65_535]));
        assert!(complete_event_fits(&vec![b'x'; 65_536]));
        assert!(!complete_event_fits(&vec![b'x'; 65_537]));
    }
}
