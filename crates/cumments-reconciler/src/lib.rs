use anyhow::Result;
use cumments_core::{
    ports::{CommentStore, IntentStore, MatrixDriver, RegistryStore},
    protocol::REDACTION_PROOF_KEY,
    site_service::SiteService,
};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use tokio::sync::Notify;

/// How long an intent may sit in `waiting_for_sync` (event sent, projection
/// not observed) before the timeout reconciliation pass intervenes.
const WAITING_FOR_SYNC_TIMEOUT_MINUTES: i64 = 10;
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
        let post_count = self.reconcile_posts().await?;
        let delete_count = self.reconcile_deletions().await?;
        let update_count = self.reconcile_updates().await?;
        let timeout_count = self.reconcile_timeouts().await?;
        Ok(post_count + delete_count + update_count + timeout_count)
    }

    async fn reconcile_posts(&self) -> Result<u64> {
        let intents = self.intent_store.get_pending_post_intents().await?;

        if intents.is_empty() {
            return Ok(0);
        }
        let num_intents = intents.len() as u64;

        for (id, intent) in intents {
            let process_result = run_intent(async {
                // ORCHESTRATION START
                // 1. Brain: Ensure the site space is ready
                let space_id = self
                    .site_service
                    .ensure_space(&intent.site_id, self.driver.as_ref())
                    .await?;

                // 2. Registry: Check for existing room in local cache (O(1) hint)
                let candidate_room_id = self
                    .registry_store
                    .get_registered_room(&intent.site_id, &intent.post_slug)
                    .await?;

                // 3. Hands: Ensure the post-specific room exists and is linked
                let room_id = self
                    .driver
                    .ensure_comment_room(
                        &intent.site_id,
                        &intent.post_slug,
                        &space_id,
                        candidate_room_id.as_deref(),
                    )
                    .await?;

                // 3b. Registry: Write back the room mapping immediately
                // This is critical in AppService mode: pushes resolve room
                // identity from this registry, so the mapping must be durable
                // before any event can arrive.
                self.registry_store
                    .register_room(&room_id, &intent.site_id, &intent.post_slug)
                    .await?;

                // 3c. Resolve the replied-to comment from the read model so
                // the Matrix message can carry a rich-reply fallback quote.
                // Unknown originals (e.g. replies to a not-yet-projected
                // event) simply skip the quote.
                let (reply_to_body, reply_to_sender) = match intent.reply_to.as_deref() {
                    Some(event_id) => match self.comment_store.get_comment(event_id).await? {
                        Some(comment) => (Some(comment.content), Some(comment.sender_mxid)),
                        None => (None, None),
                    },
                    None => (None, None),
                };

                // 4. Hands: Post the actual message
                let event_id = match self
                    .driver
                    .post_message(
                        &room_id,
                        &intent.content,
                        &intent.nickname,
                        &intent.author_public_key,
                        &intent.author_signature,
                        &intent.author_challenge,
                        &intent.site_id,
                        intent.reply_to.as_deref(),
                        reply_to_body.as_deref(),
                        reply_to_sender.as_deref(),
                        Some(id),
                    )
                    .await
                {
                    Ok(event_id) => event_id,
                    Err(e) => {
                        // The room may have been tombstoned or its alias moved:
                        // drop the registry hint so the next retry goes through
                        // alias recovery and adopts the successor room.
                        if room_unavailable(&e) {
                            warn!(
                                "Post intent [{}] failed on room {}; invalidating registry entry: {:#}",
                                id, room_id, e
                            );
                            let _ = self
                                .registry_store
                                .invalidate_room_registry(&room_id)
                                .await;
                        }
                        return Err(e);
                    }
                };

                // 5. Closed-loop: Mark as waiting for sync instead of completed
                self.intent_store
                    .mark_post_intent_waiting_for_sync(id, &event_id, &room_id)
                    .await?;
                // ORCHESTRATION END

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                let retrying = self
                    .intent_store
                    .record_post_intent_failure(id, &e.to_string())
                    .await?;
                if retrying {
                    warn!(
                        "Post intent [{}] failed, will retry after backoff: {:?}",
                        id, e
                    );
                } else {
                    error!(
                        "Post intent [{}] exhausted retries, moved to failed: {:?}",
                        id, e
                    );
                }
            }
        }
        Ok(num_intents)
    }

    async fn reconcile_deletions(&self) -> Result<u64> {
        let intents = self.intent_store.get_pending_delete_intents().await?;

        if intents.is_empty() {
            return Ok(0);
        }
        let num_intents = intents.len() as u64;

        for (id, intent) in intents {
            let process_result = run_intent(async {
                // 1. Registry: Locate the room ID for this deletion
                let room_id = self
                    .registry_store
                    .get_registered_room(&intent.site_id, &intent.post_slug)
                    .await?;

                let room_id = room_id.ok_or_else(|| {
                    anyhow::anyhow!(
                        "Room not found in registry for site {} post {}",
                        intent.site_id.as_str(),
                        intent.post_slug.as_str()
                    )
                })?;

                // 2. Hands: Perform the redaction
                let proof = serde_json::json!({
                    (REDACTION_PROOF_KEY): {
                        "site_id": intent.site_id.as_str(),
                        "post_slug": intent.post_slug.as_str(),
                        "target_event_id": intent.event_id.as_str(),
                        "public_key": intent.author_public_key.as_str(),
                        "signature": intent.author_signature.as_str(),
                        "challenge": intent.author_challenge.as_str(),
                    }
                });
                self.driver
                    .redact_message(&room_id, &intent.event_id, Some(id), Some(&proof))
                    .await?;

                // 3. Concepts: Move to waiting_for_sync
                self.intent_store
                    .mark_delete_intent_waiting_for_sync(id, &room_id)
                    .await?;

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                let retrying = self
                    .intent_store
                    .record_delete_intent_failure(id, &e.to_string())
                    .await?;
                if retrying {
                    warn!(
                        "Delete intent [{}] failed, will retry after backoff: {:?}",
                        id, e
                    );
                } else {
                    error!(
                        "Delete intent [{}] exhausted retries, moved to failed: {:?}",
                        id, e
                    );
                }
            }
        }
        Ok(num_intents)
    }

    async fn reconcile_updates(&self) -> Result<u64> {
        let intents = self.intent_store.get_pending_update_intents().await?;

        if intents.is_empty() {
            return Ok(0);
        }
        let num_intents = intents.len() as u64;

        for (id, intent) in intents {
            let process_result = run_intent(async {
                // 1. Brain: Ensure site space
                let space_id = self
                    .site_service
                    .ensure_space(&intent.site_id, self.driver.as_ref())
                    .await?;

                // 2. Registry: Check for existing room in local cache (O(1) hint)
                let candidate_room_id = self
                    .registry_store
                    .get_registered_room(&intent.site_id, &intent.post_slug)
                    .await?;

                // 3. Hands: Ensure room
                let room_id = self
                    .driver
                    .ensure_comment_room(
                        &intent.site_id,
                        &intent.post_slug,
                        &space_id,
                        candidate_room_id.as_deref(),
                    )
                    .await?;

                // 3b. Registry: Write back the room mapping immediately
                self.registry_store
                    .register_room(&room_id, &intent.site_id, &intent.post_slug)
                    .await?;

                // 4. Hands: Fetch original nickname to maintain it
                let nickname = self
                    .comment_store
                    .get_author_nickname(&intent.event_id)
                    .await?
                    .unwrap_or_else(|| "Guest".to_string());

                // 5. Hands: Perform the update (m.replace)
                self.driver
                    .update_message(
                        &room_id,
                        &intent.event_id,
                        &intent.content,
                        &nickname,
                        &intent.author_public_key,
                        &intent.author_signature,
                        &intent.author_challenge,
                        &intent.site_id,
                        Some(id),
                    )
                    .await?;

                // 6. Closed-loop: Mark as waiting for sync
                self.intent_store
                    .mark_update_intent_waiting_for_sync(id, &room_id)
                    .await?;

                Ok(())
            })
            .await;

            if let Err(e) = process_result {
                let retrying = self
                    .intent_store
                    .record_update_intent_failure(id, &e.to_string())
                    .await?;
                if retrying {
                    warn!(
                        "Update intent [{}] failed, will retry after backoff: {:?}",
                        id, e
                    );
                } else {
                    error!(
                        "Update intent [{}] exhausted retries, moved to failed: {:?}",
                        id, e
                    );
                }
            }
        }
        Ok(num_intents)
    }

    /// Handles intents stuck in `waiting_for_sync`: the event was sent but the
    /// projector never observed it (lost push, misconfigured AS, ...).
    ///
    /// - Post intents are only rescheduled when the event is confirmed absent
    ///   on the homeserver; if the event exists, resending would duplicate the
    ///   comment, so the intent is dead-lettered for inspection.
    /// - Delete/update intents are rescheduled directly: redaction and
    ///   replacement are idempotent operations.
    async fn reconcile_timeouts(&self) -> Result<u64> {
        let cutoff =
            chrono::Utc::now() - chrono::Duration::minutes(WAITING_FOR_SYNC_TIMEOUT_MINUTES);
        let mut handled = 0u64;

        for (id, event_id, room_id) in self.intent_store.get_stuck_post_intents(cutoff).await? {
            handled += 1;

            let Some(room_id) = room_id else {
                error!(
                    "Post intent [{}] timed out with no room recorded; dead-lettering",
                    id
                );
                self.intent_store
                    .dead_letter_post_intent(
                        id,
                        "waiting_for_sync timed out; room_id was not recorded, cannot verify the event safely",
                    )
                    .await?;
                continue;
            };

            if event_id.is_empty() {
                error!(
                    "Post intent [{}] timed out with no event id recorded; dead-lettering",
                    id
                );
                self.intent_store
                    .dead_letter_post_intent(
                        id,
                        "waiting_for_sync timed out; event_id was not recorded, cannot verify the event safely",
                    )
                    .await?;
                continue;
            }

            match self.driver.event_exists(&room_id, &event_id).await {
                Ok(true) => {
                    error!(
                        "Post intent [{}] timed out but event {} exists on the homeserver; dead-lettering",
                        id, event_id
                    );
                    self.intent_store
                        .dead_letter_post_intent(
                            id,
                            &format!(
                                "waiting_for_sync timed out; event {} exists on the homeserver but was never projected",
                                event_id
                            ),
                        )
                        .await?;
                }
                Ok(false) => {
                    warn!(
                        "Post intent [{}] timed out and event {} is absent; rescheduling",
                        id, event_id
                    );
                    let retrying = self
                        .intent_store
                        .record_post_intent_failure(
                            id,
                            "waiting_for_sync timed out; event absent, resending",
                        )
                        .await?;
                    if !retrying {
                        error!("Post intent [{}] exhausted retries after timeout", id);
                    }
                }
                Err(e) => {
                    warn!(
                        "Post intent [{}] timeout check failed: {:?}; leaving for next pass",
                        id, e
                    );
                }
            }
        }

        // Redaction and replacement are idempotent, so rescheduling is safe.
        for id in self
            .intent_store
            .get_stuck_delete_intent_ids(cutoff)
            .await?
        {
            handled += 1;
            let retrying = self
                .intent_store
                .record_delete_intent_failure(id, "waiting_for_sync timed out; rescheduling")
                .await?;
            if !retrying {
                error!("Delete intent [{}] exhausted retries after timeout", id);
            }
        }

        for id in self
            .intent_store
            .get_stuck_update_intent_ids(cutoff)
            .await?
        {
            handled += 1;
            let retrying = self
                .intent_store
                .record_update_intent_failure(id, "waiting_for_sync timed out; rescheduling")
                .await?;
            if !retrying {
                error!("Update intent [{}] exhausted retries after timeout", id);
            }
        }

        Ok(handled)
    }
}
