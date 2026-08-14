mod decommission;
mod deletions;
mod media;
mod moderation;
mod pass;
mod posts;
mod timeouts;
mod updates;

use anyhow::Result;
use cumments_core::{
    matrix_error::MatrixError,
    models::{PostSlug, QuarantinedRoom, SiteId},
    ports::{
        GovernanceStore, MatrixDriver, MessageStore, RegistryStore, RoleClaimStore, SiteAuthStore,
        SiteStore, SubmissionStore, VirtualUserStore,
    },
    site_service::SiteService,
};
use pass::{PassConfig, ReconcilePass};
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

use tokio::sync::Notify;

/// How long a submission may sit in `waiting_for_sync` (event sent, projection
/// not observed) before the timeout reconciliation pass intervenes.
const WAITING_FOR_SYNC_TIMEOUT_MINUTES: i64 = 10;
/// How many consecutive timeout passes must observe the event as existing
/// before the submission is dead-lettered. Projection can be delayed by push
/// retries or restarts, so a single confirmation is not treated as failure.
const TIMEOUT_CONFIRMATION_LIMIT: u32 = 3;
/// Consecutive event-existence check failures before dead-lettering a post
/// submission; prevent indefinite limbo on persistent homeserver errors.
const TIMEOUT_ERROR_LIMIT: u32 = 5;
/// Maximum number of pending submissions loaded per queue per pass. Keeps memory
/// bounded under write floods.
const SUBMISSION_BATCH_SIZE: u64 = 100;
/// Upper bound for processing a single submission, including all Matrix driver
/// calls (room creation, joins, sends). Prevents one stuck homeserver request
/// from stalling the whole write path.
const SUBMISSION_TIMEOUT: Duration = Duration::from_secs(90);
/// How long a claimed submission stays `processing` before a crashed pass's
/// lease expires and the row becomes claimable again.
const SUBMISSION_LEASE: Duration = Duration::from_secs(5 * 60);
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
/// Resync interval for submission passes; wakeups keep them prompt.
const SUBMISSION_PASS_INTERVAL: Duration = Duration::from_secs(5);
/// Resync interval for governance passes.
const GOVERNANCE_PASS_INTERVAL: Duration = Duration::from_secs(60);
/// Interval for the orphan-media sweep; it has no event source, so the
/// interval alone drives it.
const MEDIA_CLEANUP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Run one submission.s processing future with a hard time budget.
async fn run_submission<F>(future: F) -> Result<()>
where
    F: std::future::Future<Output = Result<()>>,
{
    match tokio::time::timeout(SUBMISSION_TIMEOUT, future).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "submission processing timed out after {:?}",
            SUBMISSION_TIMEOUT
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

/// Shared dependencies of every reconcile pass. Each pass is scheduled by
/// the [`Reconciler`]; none owns Matrix-write authority beyond the driver.
pub struct ReconcilerDeps {
    pub submission_store: Arc<dyn SubmissionStore>,
    pub registry_store: Arc<dyn RegistryStore>,
    pub site_store: Arc<dyn SiteStore>,
    pub role_claim_store: Arc<dyn RoleClaimStore>,
    pub governance_store: Arc<dyn GovernanceStore>,
    pub message_store: Arc<dyn MessageStore>,
    pub virtual_user_store: Arc<dyn VirtualUserStore>,
    pub site_auth_store: Arc<dyn SiteAuthStore>,
    pub driver: Arc<dyn MatrixDriver>,
    pub site_service: Arc<SiteService>,
}

/// The event sources that wake the reconcile passes. Each pass subscribes to
/// exactly one; routing is a first-class part of the design.
pub struct PassWakeups {
    /// API-saved comment write submissions.
    pub submission: Arc<Notify>,
    /// API-side governance writes (role changes, site retirement).
    pub governance: Arc<Notify>,
    /// Projection-driven governance changes (token-DM activation, Space
    /// power-level pushes).
    pub projection: Arc<Notify>,
}

/// The reconciler: a set of independent [`ReconcilePass`] controllers.
/// It owns no scheduling logic of its own beyond spawning one task per pass.
pub struct Reconciler {
    passes: Vec<Arc<dyn ReconcilePass>>,
}

impl Reconciler {
    pub fn new(deps: ReconcilerDeps, wakeups: PassWakeups) -> Self {
        let deps = Arc::new(deps);
        let schedule = pass_schedule(&wakeups);

        let passes: Vec<Arc<dyn ReconcilePass>> = vec![
            Arc::new(posts::PostsPass::new(deps.clone(), schedule.posts)),
            Arc::new(deletions::DeletionsPass::new(
                deps.clone(),
                schedule.deletions,
            )),
            Arc::new(updates::UpdatesPass::new(deps.clone(), schedule.updates)),
            Arc::new(timeouts::TimeoutsPass::new(deps.clone(), schedule.timeouts)),
            Arc::new(moderation::ClaimsPass::new(deps.clone(), schedule.claims)),
            Arc::new(moderation::ModerationPass::new(
                deps.clone(),
                schedule.moderation,
            )),
            Arc::new(decommission::DecommissionPass::new(
                deps.clone(),
                schedule.decommission,
            )),
            Arc::new(media::MediaCleanupPass::new(
                deps.clone(),
                PassConfig {
                    name: "media-cleanup",
                    interval: MEDIA_CLEANUP_INTERVAL,
                    // No producer ever signals this channel; the interval is
                    // the pass's only driver.
                    wakeup: Arc::new(Notify::new()),
                },
            )),
        ];
        Self { passes }
    }

    /// Spawns one task per pass and waits forever. Pass failures are logged
    /// per pass and never stop the others.
    pub async fn run(&self) {
        info!("Starting {} reconcile passes.", self.passes.len());
        for pass in &self.passes {
            tokio::spawn(pass::run_pass(Arc::clone(pass)));
        }
        std::future::pending::<()>().await;
    }
}

/// The pass-to-wakeup routing table, kept as a pure function so the routing
/// is unit-testable without constructing any store mocks.
struct PassSchedule {
    posts: PassConfig,
    deletions: PassConfig,
    updates: PassConfig,
    timeouts: PassConfig,
    claims: PassConfig,
    moderation: PassConfig,
    decommission: PassConfig,
}

fn pass_schedule(wakeups: &PassWakeups) -> PassSchedule {
    let submission = |name: &'static str| PassConfig {
        name,
        interval: SUBMISSION_PASS_INTERVAL,
        wakeup: wakeups.submission.clone(),
    };
    let governance = |name: &'static str| PassConfig {
        name,
        interval: GOVERNANCE_PASS_INTERVAL,
        wakeup: wakeups.governance.clone(),
    };
    let projection = |name: &'static str| PassConfig {
        name,
        interval: GOVERNANCE_PASS_INTERVAL,
        wakeup: wakeups.projection.clone(),
    };

    PassSchedule {
        posts: submission("posts"),
        deletions: submission("deletions"),
        updates: submission("updates"),
        timeouts: submission("timeouts"),
        claims: projection("claims"),
        moderation: projection("moderation"),
        decommission: governance("decommission"),
    }
}

/// The quarantined room for a site/post, if any.
async fn quarantined_room_for(
    deps: &ReconcilerDeps,
    site_id: &SiteId,
    post_slug: &PostSlug,
) -> Result<Option<QuarantinedRoom>> {
    let rooms = deps.registry_store.get_quarantined_rooms().await?;
    Ok(rooms
        .into_iter()
        .find(|r| r.site_id == site_id.as_str() && r.post_slug == post_slug.as_str()))
}

/// Records one more adoption failure for a room, applying the backoff
/// schedule and escalating to manual attention after repeated failures.
async fn record_adoption_failure(deps: &ReconcilerDeps, room_id: &str, reason: &str) -> Result<()> {
    let failures = deps
        .registry_store
        .get_quarantined_rooms()
        .await?
        .into_iter()
        .find(|r| r.room_id == room_id)
        .map(|r| r.adoption_failures + 1)
        .unwrap_or(1);
    deps.registry_store
        .quarantine_room(room_id, reason, failures, next_quarantine_attempt(failures))
        .await?;
    Ok(())
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

    #[test]
    fn pass_schedule_routes_each_wakeup() {
        let submission = Arc::new(Notify::new());
        let governance = Arc::new(Notify::new());
        let projection = Arc::new(Notify::new());
        let schedule = pass_schedule(&PassWakeups {
            submission: submission.clone(),
            governance: governance.clone(),
            projection: projection.clone(),
        });

        for config in [
            &schedule.posts,
            &schedule.deletions,
            &schedule.updates,
            &schedule.timeouts,
        ] {
            assert_eq!(config.interval, SUBMISSION_PASS_INTERVAL);
            assert!(Arc::ptr_eq(&config.wakeup, &submission), "{}", config.name);
        }
        for config in [&schedule.claims, &schedule.moderation] {
            assert_eq!(config.interval, GOVERNANCE_PASS_INTERVAL);
            assert!(Arc::ptr_eq(&config.wakeup, &projection), "{}", config.name);
        }
        assert_eq!(schedule.decommission.interval, GOVERNANCE_PASS_INTERVAL);
        assert!(Arc::ptr_eq(&schedule.decommission.wakeup, &governance));
    }
}
