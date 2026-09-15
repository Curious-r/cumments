//! Ownership-release sweep for Cumments-managed media.
//!
//! The homeserver remains the authority on physical media lifecycle. This pass
//! never deletes Matrix media; it only releases Cumments' own upload ownership
//! records once an upload is old enough and reachability evaluation proves it
//! is no longer referenced by any semantic source. Reachability that cannot be
//! determined (`Unknown`) always keeps the ownership record.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::media_reachability::MediaReachabilityEvaluator;

/// Grace period before an upload's ownership may be released, anchored on the
/// upload record's `created_at`.
const ORPHAN_GRACE_PERIOD: chrono::Duration = chrono::Duration::hours(24);

/// Periodic ownership-release sweep over old Cumments-owned uploads.
pub struct MediaCleanupPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl MediaCleanupPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        Self { deps, config }
    }

    async fn reconcile(&self) -> Result<u64> {
        let evaluator = MediaReachabilityEvaluator::new(
            self.deps.driver.clone(),
            self.deps.message_store.clone(),
        );

        let cutoff = chrono::Utc::now() - ORPHAN_GRACE_PERIOD;
        let candidates = self
            .deps
            .message_store
            .list_media_upload_candidates_before(cutoff)
            .await?;

        let mut released = 0u64;
        for candidate in candidates {
            let site_id = SiteId::from(candidate.site_id.clone());
            let evaluation = evaluator
                .evaluate_candidate(&site_id, &candidate.mxc_url)
                .await;
            if !evaluation.is_cleanup_eligible() {
                continue;
            }

            match self
                .deps
                .message_store
                .release_media_upload_ownership(
                    &candidate.site_id,
                    &candidate.mxc_url,
                    candidate.id,
                )
                .await
            {
                Ok(true) => {
                    released += 1;
                    tracing::info!(
                        site_id = %candidate.site_id,
                        mxc = %candidate.mxc_url,
                        "released media upload ownership"
                    );
                }
                // The row was already released by a concurrent cleanup run.
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        site_id = %candidate.site_id,
                        mxc = %candidate.mxc_url,
                        "failed to release media upload ownership: {error:#}"
                    );
                }
            }
        }
        Ok(released)
    }
}

#[async_trait]
impl ReconcilePass for MediaCleanupPass {
    fn config(&self) -> &PassConfig {
        &self.config
    }

    async fn run(&self) -> Result<u64> {
        self.reconcile().await
    }
}
