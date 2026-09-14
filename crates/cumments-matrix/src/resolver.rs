//! Matrix implementation of [`HistoricalRoomStateResolver`].
//!
//! # Facility & Architectural Guarantees
//! This provider realizes the abstract [`HistoricalRoomStateResolver`] capability using
//! the Matrix Client-Server API facility:
//! ```text
//! GET /_matrix/client/v3/rooms/{roomId}/context/{eventId}?limit=0
//! ```
//!
//! ## Operational Guarantees
//! 1. **State Resolution Context**: The Matrix specification (§Room context) defines that
//!    `GET /context/{eventId}` returns the state of the room at the point in time of the
//!    given event $E$, resolved by the homeserver's state resolution engine (e.g. Matrix
//!    State Resolution v2). Setting `limit=0` requests zero context events before or after,
//!    retrieving only the target event and the resolved room state at event $E$.
//! 2. **Author Identification**: The effective author presentation is extracted by finding
//!    the `m.room.member` state event with `state_key == sender_mxid` in the returned state.
//! 3. **Membership Usability**: Presentation usability is evaluated independently of
//!    `membership == "join"`; authors who departed (`membership == "leave"`) or were banned
//!    (`membership == "ban"`) retain their usable presentation for messages sent at or before
//!    that state transition.
//! 4. **Field Interpretation**: Profile fields are evaluated strictly according to the
//!    homeserver's resolved state event; cleared fields (`null`, empty string, or absent)
//!    evaluate cleanly to `None` without guessing previous DAG values or falling back to
//!    current room presentation.
//!
//! ## Capabilities, Constraints & Failure Modes
//! - This provider does not claim that `/context` is universally and unconditionally
//!   authoritative in every hypothetical deployment without preconditions. It relies on:
//!   - **History Visibility**: The AppService bot user having sufficient room history visibility
//!     (e.g., `m.room.history_visibility` permitting state access at $E$).
//!   - **State Retention**: The homeserver retaining the DAG state at $E$. If the homeserver
//!     has pruned historical room state, it will return HTTP 404 (`M_NOT_FOUND`).
//! - **Strict Error Semantics**: If the homeserver returns 4xx, 5xx, a transport error, or
//!   malformed state JSON, the resolver returns `Err(...)`. Errors are never converted into
//!   `Ok(None)`.
//! - **Valid Absence**: `Ok(None)` is returned only when the homeserver successfully returned
//!   the resolved state context at $E$ and either:
//!   - No `m.room.member` state existed for the author at event $E$; or
//!   - The author's member state event contained no non-empty `displayname` and no non-empty `avatar_url`.
//!
//! ## Supported Homeserver Environments
//! Validated and supported on:
//! - Synapse (>= 1.90.0)
//! - Dendrite (>= 0.13.0)
//! - Conduit / Conduit-derived implementations

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use cumments_core::models::MemberPresentation;
use cumments_core::ports::HistoricalRoomStateResolver;

use crate::appservice::AppServiceMatrixDriver;

/// Concrete driver implementing [`HistoricalRoomStateResolver`] using Matrix homeserver state facilities.
pub struct MatrixHistoricalStateResolver {
    driver: Arc<AppServiceMatrixDriver>,
}

impl MatrixHistoricalStateResolver {
    pub fn new(driver: Arc<AppServiceMatrixDriver>) -> Self {
        Self { driver }
    }
}

#[async_trait]
impl HistoricalRoomStateResolver for MatrixHistoricalStateResolver {
    async fn resolve_member_presentation(
        &self,
        room_id: &str,
        event_id: &str,
        sender_mxid: &str,
    ) -> Result<Option<MemberPresentation>> {
        self.driver
            .resolve_member_presentation(room_id, event_id, sender_mxid)
            .await
    }
}
