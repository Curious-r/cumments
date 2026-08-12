mod deletions;
mod posts;
mod timeouts;
mod updates;

use anyhow::Result;
use cumments_core::{
    ports::{CommentStore, IntentStore, MatrixDriver, RegistryStore},
    site_service::SiteService,
};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

use tokio::sync::Notify;

/// How long an intent may sit in `waiting_for_sync` (event sent, projection
/// not observed) before the timeout reconciliation pass intervenes.
const WAITING_FOR_SYNC_TIMEOUT_MINUTES: i64 = 10;
/// How many consecutive timeout passes must observe the event as existing
/// before the intent is dead-lettered. Projection can be delayed by push
/// retries or restarts, so a single confirmation is not treated as failure.
const TIMEOUT_CONFIRMATION_LIMIT: u32 = 3;
/// Consecutive event-existence check failures before dead-lettering a post
/// intent; prevents indefinite limbo on persistent homeserver errors.
const TIMEOUT_ERROR_LIMIT: u32 = 5;
/// Maximum number of pending intents loaded per queue per pass. Keeps memory
/// bounded under write floods; `reconcile()` loops until a batch is empty.
const INTENT_BATCH_SIZE: u64 = 100;
/// Upper bound for processing a single intent, including all Matrix driver
/// calls (room creation, joins, sends). Prevents one stuck homeserver request
/// from stalling the whole write path.
const INTENT_TIMEOUT: Duration = Duration::from_secs(90);

/// Run one intent's processing future with a hard time budget.
async fn run_intent<F>(future: F) -> Result<()>
where
    F: std::future::Future<Output = Result<()>>,
{
    match tokio::time::timeout(INTENT_TIMEOUT, future).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "intent processing timed out after {:?}",
            INTENT_TIMEOUT
        )),
    }
}

/// Whether a driver error indicates the target room itself is gone or no
/// longer writable (as opposed to a transient homeserver failure).
fn room_unavailable(err: &anyhow::Error) -> bool {
    let text = err.to_string();
    text.contains("M_NOT_FOUND") || text.contains("M_FORBIDDEN")
}

/// Whether a driver error means a room was refused/blocked during adoption
/// (governance or room-version validation), as opposed to a transient
/// failure. These should be surfaced as operator-visible blocked rooms.
fn adoption_blocked(err: &anyhow::Error) -> bool {
    let text = err.to_string();
    text.contains("Refusing to adopt")
        || text.contains("Cannot verify governance")
        || text.contains("redact threshold")
        || text.contains("not created as m.space")
}

/// The Reconciler acts as the Orchestrator of the background process.
/// It coordinates between the SiteService (Brain) and the MatrixDriver (Hands).
pub struct Reconciler {
    intent_store: Arc<dyn IntentStore>,
    registry_store: Arc<dyn RegistryStore>,
    comment_store: Arc<dyn CommentStore>,
    driver: Arc<dyn MatrixDriver>,
    site_service: Arc<SiteService>,
    notify: Arc<Notify>,
}

impl Reconciler {
    pub fn new(
        intent_store: Arc<dyn IntentStore>,
        registry_store: Arc<dyn RegistryStore>,
        comment_store: Arc<dyn CommentStore>,
        driver: Arc<dyn MatrixDriver>,
        site_service: Arc<SiteService>,
        notify: Arc<Notify>,
    ) -> Self {
        Self {
            intent_store,
            registry_store,
            comment_store,
            driver,
            site_service,
            notify,
        }
    }

    /// Runs the main reconciliation loop.
    pub async fn run(&self) {
        info!("Starting reactive reconciler loop...");
        // Fallback interval for retries and periodic cleanup
        let mut interval = tokio::time::interval(Duration::from_secs(60));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    tracing::debug!("Reconciler: Periodic scan triggered.");
                }
                _ = self.notify.notified() => {
                    tracing::debug!("Reconciler: Instant wake-up triggered by notification.");
                }
            }

            match self.reconcile().await {
                Ok(count) => {
                    if count > 0 {
                        info!("Successfully reconciled {} intents.", count);
                    }
                }
                Err(e) => {
                    tracing::error!("Reconciliation failed: {:?}", e);
                }
            }
        }
    }

    /// Reconciles all types of pending intents from the database.
    async fn reconcile(&self) -> Result<u64> {
        let mut post_count = 0;
        loop {
            match self.reconcile_posts().await {
                Ok(batch) => {
                    post_count += batch;
                    if batch < INTENT_BATCH_SIZE {
                        break;
                    }
                }
                Err(e) => {
                    error!("Reconcile posts phase failed: {:#}", e);
                    break;
                }
            }
        }

        let mut delete_count = 0;
        loop {
            match self.reconcile_deletions().await {
                Ok(batch) => {
                    delete_count += batch;
                    if batch < INTENT_BATCH_SIZE {
                        break;
                    }
                }
                Err(e) => {
                    error!("Reconcile deletions phase failed: {:#}", e);
                    break;
                }
            }
        }

        let mut update_count = 0;
        loop {
            match self.reconcile_updates().await {
                Ok(batch) => {
                    update_count += batch;
                    if batch < INTENT_BATCH_SIZE {
                        break;
                    }
                }
                Err(e) => {
                    error!("Reconcile updates phase failed: {:#}", e);
                    break;
                }
            }
        }

        let timeout_count = match self.reconcile_timeouts().await {
            Ok(n) => n,
            Err(e) => {
                error!("Reconcile timeouts phase failed: {:#}", e);
                0
            }
        };
        Ok(post_count + delete_count + update_count + timeout_count)
    }
}
