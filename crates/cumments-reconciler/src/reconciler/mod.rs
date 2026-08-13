mod decommission;
mod deletions;
mod moderation;
mod posts;
mod timeouts;
mod updates;

use anyhow::Result;
use cumments_core::{
    matrix_error::MatrixError,
    models::{PostSlug, QuarantinedRoom, SiteId},
    ports::{
        IntentStore, MatrixDriver, MessageStore, RegistryStore, RoleClaimStore, SiteAuthStore,
        SiteStore,
    },
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
/// Adoption-retry backoff schedule for quarantined rooms.
const QUARANTINE_BACKOFFS: [Duration; 3] = [
    Duration::from_secs(60 * 60),
    Duration::from_secs(6 * 60 * 60),
    Duration::from_secs(24 * 60 * 60),
];
/// Consecutive adoption failures before automatic retries stop and the room
/// requires manual `reinstate`. Failures 1-3 schedule the 1h/6h/24h retries;
/// the fourth failure escalates.
const QUARANTINE_ESCALATION: u32 = 4;

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

/// Next scheduled automatic adoption attempt after `failures` consecutive
/// adoption failures; `None` once the escalation threshold is reached.
fn next_quarantine_attempt(failures: u32) -> Option<chrono::DateTime<chrono::Utc>> {
    if failures == 0 || failures >= QUARANTINE_ESCALATION {
        return None;
    }
    QUARANTINE_BACKOFFS
        .get((failures - 1) as usize)
        .map(|backoff| chrono::Utc::now() + *backoff)
}

/// Whether a driver error means adoption was refused for governance reasons;
/// the room should be quarantined.
fn should_quarantine(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<MatrixError>(),
            Some(MatrixError::AdoptionRefused { .. })
        )
    })
}

/// Whether a driver error means the target room itself is gone or no longer
/// writable; the registry entry should be retired.
fn is_room_gone(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<MatrixError>(),
            Some(MatrixError::RoomGone { .. })
        )
    })
}

/// The room ID of an adoption refusal, if the typed error carries one.
fn quarantine_target(err: &anyhow::Error) -> Option<String> {
    err.chain()
        .find_map(|cause| match cause.downcast_ref::<MatrixError>() {
            Some(MatrixError::AdoptionRefused { room_id, .. }) => Some(room_id.clone()),
            _ => None,
        })
}

/// The Reconciler acts as the Orchestrator of the background process.
/// It coordinates between the SiteService (Brain) and the MatrixDriver (Hands).
pub struct Reconciler {
    intent_store: Arc<dyn IntentStore>,
    registry_store: Arc<dyn RegistryStore>,
    site_store: Arc<dyn SiteStore>,
    role_claim_store: Arc<dyn RoleClaimStore>,
    message_store: Arc<dyn MessageStore>,
    site_auth_store: Arc<dyn SiteAuthStore>,
    driver: Arc<dyn MatrixDriver>,
    site_service: Arc<SiteService>,
    notify: Arc<Notify>,
}

/// Dependencies of the [`Reconciler`], kept as one struct so the growing set
/// of stores stays readable at construction sites.
pub struct ReconcilerDeps {
    pub intent_store: Arc<dyn IntentStore>,
    pub registry_store: Arc<dyn RegistryStore>,
    pub site_store: Arc<dyn SiteStore>,
    pub role_claim_store: Arc<dyn RoleClaimStore>,
    pub message_store: Arc<dyn MessageStore>,
    pub site_auth_store: Arc<dyn SiteAuthStore>,
    pub driver: Arc<dyn MatrixDriver>,
    pub site_service: Arc<SiteService>,
    pub notify: Arc<Notify>,
}

impl Reconciler {
    pub fn new(deps: ReconcilerDeps) -> Self {
        Self {
            intent_store: deps.intent_store,
            registry_store: deps.registry_store,
            site_store: deps.site_store,
            role_claim_store: deps.role_claim_store,
            message_store: deps.message_store,
            site_auth_store: deps.site_auth_store,
            driver: deps.driver,
            site_service: deps.site_service,
            notify: deps.notify,
        }
    }

    /// The quarantined room for a site/post, if any.
    async fn quarantined_room_for(
        &self,
        site_id: &SiteId,
        post_slug: &PostSlug,
    ) -> Result<Option<QuarantinedRoom>> {
        let rooms = self.registry_store.get_quarantined_rooms().await?;
        Ok(rooms
            .into_iter()
            .find(|r| r.site_id == site_id.as_str() && r.post_slug == post_slug.as_str()))
    }

    /// Records one more adoption failure for a room, applying the backoff
    /// schedule and escalating to manual attention after repeated failures.
    async fn record_adoption_failure(&self, room_id: &str, reason: &str) -> Result<()> {
        let failures = self
            .registry_store
            .get_quarantined_rooms()
            .await?
            .into_iter()
            .find(|r| r.room_id == room_id)
            .map(|r| r.adoption_failures + 1)
            .unwrap_or(1);
        self.registry_store
            .quarantine_room(room_id, reason, failures, next_quarantine_attempt(failures))
            .await?;
        Ok(())
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
        let claim_count = match self.reconcile_claims().await {
            Ok(n) => n,
            Err(e) => {
                error!("Reconcile claims phase failed: {:#}", e);
                0
            }
        };
        let moderation_count = match self.reconcile_moderation().await {
            Ok(n) => n,
            Err(e) => {
                error!("Reconcile moderation phase failed: {:#}", e);
                0
            }
        };
        let decommission_count = match self.reconcile_decommissions().await {
            Ok(n) => n,
            Err(e) => {
                error!("Reconcile decommission phase failed: {:#}", e);
                0
            }
        };
        Ok(post_count
            + delete_count
            + update_count
            + timeout_count
            + claim_count
            + moderation_count
            + decommission_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarantine_backoff_escalates_after_three_scheduled_retries() {
        let now = chrono::Utc::now();
        let one_hour = next_quarantine_attempt(1).expect("1h retry") - now;
        assert!((one_hour - chrono::Duration::hours(1)).num_seconds().abs() <= 5);
        let six_hours = next_quarantine_attempt(2).expect("6h retry") - now;
        assert!((six_hours - chrono::Duration::hours(6)).num_seconds().abs() <= 5);
        let day = next_quarantine_attempt(3).expect("24h retry") - now;
        assert!((day - chrono::Duration::hours(24)).num_seconds().abs() <= 5);
        assert!(next_quarantine_attempt(4).is_none());
        assert!(next_quarantine_attempt(0).is_none());
    }

    #[test]
    fn typed_classifiers_drive_quarantine_and_retire_policy() {
        let refused = anyhow::Error::new(MatrixError::AdoptionRefused {
            room_id: "!room:hs".to_string(),
            reason: "Refusing to adopt room".to_string(),
        });
        assert!(should_quarantine(&refused));
        assert!(!is_room_gone(&refused));
        assert_eq!(quarantine_target(&refused).as_deref(), Some("!room:hs"));

        let gone = anyhow::Error::new(MatrixError::RoomGone {
            room_id: "!room:hs".to_string(),
            reason: "M_NOT_FOUND".to_string(),
        });
        assert!(!should_quarantine(&gone));
        assert!(is_room_gone(&gone));
        assert!(quarantine_target(&gone).is_none());

        let transient = anyhow::anyhow!("request timed out");
        assert!(!should_quarantine(&transient));
        assert!(!is_room_gone(&transient));
    }
}
