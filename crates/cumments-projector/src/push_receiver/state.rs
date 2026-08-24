//! Shared state for the push receiver endpoints.

use crate::event_processor::EventProcessor;
use cumments_core::ports::AppServiceTxnStore;
use std::sync::Arc;

// ── Shared state ──────────────────────────────────────────────────

/// Shared state for the push receiver endpoints.
pub struct PushState {
    pub(super) processor: Arc<EventProcessor>,
    pub(super) hs_token: String,
    /// Durable acknowledgement records survive process restarts; bounded
    /// storage means event-level idempotency remains the correctness backstop.
    pub(super) txn_store: Arc<dyn AppServiceTxnStore>,
}
