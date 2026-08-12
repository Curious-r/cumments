//! Shared state for the push receiver endpoints.

use crate::event_processor::EventProcessor;
use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Mutex},
};

// ── Shared state ──────────────────────────────────────────────────

/// Shared state for the push receiver endpoints.
pub struct PushState {
    pub(super) processor: Arc<EventProcessor>,
    pub(super) hs_token: String,
    /// Transaction IDs already acknowledged successfully. Prevents a
    /// homeserver retry of a fully processed transaction from re-broadcasting
    /// the same events over SSE.
    pub(super) processed_txns: Mutex<ProcessedTxnSet>,
}

/// Upper bound on remembered transaction IDs; the oldest entry is evicted
/// beyond it so memory stays bounded without clearing the whole set.
const MAX_PROCESSED_TXNS: usize = 10_000;

/// Bounded FIFO of recently acknowledged transaction IDs. Evicting the oldest
/// ID instead of clearing the whole set keeps a transient homeserver retry of
/// a slightly older transaction from re-broadcasting SSE events.
pub(super) struct ProcessedTxnSet {
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl ProcessedTxnSet {
    pub(super) fn new() -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    pub(super) fn contains(&self, txn_id: &str) -> bool {
        self.seen.contains(txn_id)
    }

    pub(super) fn insert(&mut self, txn_id: String) {
        if !self.seen.insert(txn_id.clone()) {
            return;
        }
        self.order.push_back(txn_id);
        while self.order.len() > MAX_PROCESSED_TXNS {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processed_txn_set_evicts_oldest_instead_of_clearing_all() {
        let mut set = ProcessedTxnSet::new();
        for i in 0..=MAX_PROCESSED_TXNS {
            set.insert(format!("txn-{i}"));
        }
        assert!(!set.contains("txn-0"));
        assert!(set.contains(&format!("txn-{MAX_PROCESSED_TXNS}")));
        assert_eq!(set.order.len(), MAX_PROCESSED_TXNS);
    }
}
