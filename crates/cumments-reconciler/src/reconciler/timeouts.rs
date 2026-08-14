//! Timeout reconciliation for submissions stuck in `waiting_for_sync`.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use tracing::{error, warn};

/// Reschedules or dead-letters submissions stuck in `waiting_for_sync`.
pub struct TimeoutsPass {
    deps: Arc<ReconcilerDeps>,
    config: PassConfig,
}

impl TimeoutsPass {
    pub fn new(deps: Arc<ReconcilerDeps>, config: PassConfig) -> Self {
        Self { deps, config }
    }

    async fn reconcile(&self) -> Result<u64> {
        let cutoff =
            chrono::Utc::now() - chrono::Duration::minutes(WAITING_FOR_SYNC_TIMEOUT_MINUTES);
        let mut handled = 0u64;

        loop {
            let stuck_batch = self
                .deps
                .submission_store
                .get_stuck_post_submissions(cutoff, SUBMISSION_BATCH_SIZE)
                .await?;
            if stuck_batch.is_empty() {
                break;
            }
            for stuck in stuck_batch {
                let id = stuck.id;
                let event_id = stuck.event_id;
                let room_id = stuck.room_id;
                handled += 1;

                let Some(room_id) = room_id else {
                    error!(
                        "Post submission [{}] timed out with no room recorded; dead-lettering",
                        id
                    );
                    self.deps
                        .submission_store
                        .dead_letter_post_submission(
                            id,
                            "waiting_for_sync timed out; room_id was not recorded, cannot verify the event safely",
                        )
                        .await?;
                    continue;
                };

                if event_id.is_empty() {
                    error!(
                        "Post submission [{}] timed out with no event id recorded; dead-lettering",
                        id
                    );
                    self.deps
                        .submission_store
                        .dead_letter_post_submission(
                            id,
                            "waiting_for_sync timed out; event_id was not recorded, cannot verify the event safely",
                        )
                        .await?;
                    continue;
                }

                match self.deps.driver.event_exists(&room_id, &event_id).await {
                    Ok(true) => {
                        self.deps
                            .submission_store
                            .reset_post_timeout_errors(id)
                            .await?;
                        let confirmations = self
                            .deps
                            .submission_store
                            .increment_post_timeout_confirmation(id)
                            .await?;
                        if confirmations >= TIMEOUT_CONFIRMATION_LIMIT {
                            error!(
                                "Post submission [{}] timed out and event {} exists on the homeserver; dead-lettering after {confirmations} confirmations",
                                id, event_id
                            );
                            self.deps
                                .submission_store
                                .dead_letter_post_submission(
                                    id,
                                    &format!(
                                        "waiting_for_sync timed out; event {} exists on the homeserver but was never projected after {confirmations} confirmation passes",
                                        event_id
                                    ),
                                )
                                .await?;
                        } else {
                            warn!(
                                "Post submission [{}] event {} exists but projection is delayed; confirmation {confirmations}/{TIMEOUT_CONFIRMATION_LIMIT}",
                                id, event_id
                            );
                        }
                    }
                    Ok(false) => {
                        self.deps
                            .submission_store
                            .reset_post_timeout_errors(id)
                            .await?;
                        warn!(
                            "Post submission [{}] timed out and event {} is absent; rescheduling",
                            id, event_id
                        );
                        self.deps
                            .submission_store
                            .reset_post_timeout_confirmations(id)
                            .await?;
                        self.deps
                            .submission_store
                            .mark_post_submission_force_new_txn(id)
                            .await?;
                        let retrying = self
                            .deps
                            .submission_store
                            .record_post_submission_failure(
                                id,
                                "waiting_for_sync timed out; event absent, resending",
                            )
                            .await?;
                        if !retrying {
                            error!("Post submission [{}] exhausted retries after timeout", id);
                        }
                    }
                    Err(e) => {
                        let errors = self
                            .deps
                            .submission_store
                            .increment_post_timeout_error(id)
                            .await?;
                        if errors >= TIMEOUT_ERROR_LIMIT {
                            error!(
                                "Post submission [{}] timeout check failed {errors} times; dead-lettering: {:?}",
                                id, e
                            );
                            self.deps
                                .submission_store
                                .dead_letter_post_submission(
                                    id,
                                    &format!(
                                        "waiting_for_sync timeout check failed {errors} consecutive times: {e}"
                                    ),
                                )
                                .await?;
                        } else {
                            warn!(
                                "Post submission [{}] timeout check failed (error {errors}/{TIMEOUT_ERROR_LIMIT}): {:?}",
                                id, e
                            );
                        }
                    }
                }
            }
        }

        // Redaction and replacement are idempotent, so rescheduling is safe.
        loop {
            let ids = self
                .deps
                .submission_store
                .get_stuck_delete_submission_ids(cutoff, SUBMISSION_BATCH_SIZE)
                .await?;
            if ids.is_empty() {
                break;
            }
            for id in ids {
                handled += 1;
                let retrying = self
                    .deps
                    .submission_store
                    .record_delete_submission_failure(
                        id,
                        "waiting_for_sync timed out; rescheduling",
                    )
                    .await?;
                if !retrying {
                    error!("Delete submission [{}] exhausted retries after timeout", id);
                }
            }
        }

        loop {
            let ids = self
                .deps
                .submission_store
                .get_stuck_update_submission_ids(cutoff, SUBMISSION_BATCH_SIZE)
                .await?;
            if ids.is_empty() {
                break;
            }
            for id in ids {
                handled += 1;
                let retrying = self
                    .deps
                    .submission_store
                    .record_update_submission_failure(
                        id,
                        "waiting_for_sync timed out; rescheduling",
                    )
                    .await?;
                if !retrying {
                    error!("Update submission [{}] exhausted retries after timeout", id);
                }
            }
        }

        Ok(handled)
    }
}

#[async_trait]
impl ReconcilePass for TimeoutsPass {
    fn config(&self) -> &PassConfig {
        &self.config
    }

    async fn run(&self) -> Result<u64> {
        self.reconcile().await
    }
}
