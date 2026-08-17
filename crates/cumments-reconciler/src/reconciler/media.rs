//! Orphan media cleanup: forget uploads that were never referenced by a
//! comment, deleting the homeserver copy best-effort.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use tracing::{info, warn};

/// An upload counts as orphaned once it has been unreferenced this long.
const ORPHAN_AGE: chrono::Duration = chrono::Duration::hours(24);

/// Periodic sweep for unreferenced visitor uploads.
pub struct MediaCleanupPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl MediaCleanupPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        Self { deps, config }
    }

    async fn reconcile(&self) -> Result<u64> {
        let cutoff = chrono::Utc::now() - ORPHAN_AGE;
        let orphans = self
            .deps
            .message_store
            .list_unused_media_before(cutoff)
            .await?;
        let mut cleaned = 0u64;
        for url in orphans {
            let Some(rest) = url.strip_prefix("mxc://") else {
                continue;
            };
            let Some((server, media_id)) = rest.split_once('/') else {
                continue;
            };
            match self.deps.driver.delete_media(server, media_id).await {
                Ok(true) => match self.deps.message_store.delete_media_upload(&url).await {
                    Ok(()) => {
                        cleaned += 1;
                        info!(url, "orphan media deleted");
                    }
                    Err(error) => {
                        warn!(url, "failed to forget orphan media: {error:#}");
                    }
                },
                Ok(false) => {
                    warn!(url, "homeserver refused deletion; keeping record");
                }
                Err(error) => {
                    warn!(url, "orphan media deletion failed: {error:#}");
                }
            }
        }
        Ok(cleaned)
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
