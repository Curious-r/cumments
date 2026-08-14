use chrono::{Duration, Utc};
use cumments_core::{
    commands::{DeleteCommentCommand, PostCommentCommand, UpdateCommentCommand},
    models::{PostSlug, SiteId},
    ports::SubmissionStore,
};
use cumments_store::DbStore;

fn lease(duration: chrono::Duration) -> chrono::DateTime<Utc> {
    Utc::now() + duration
}

fn post_command() -> PostCommentCommand {
    PostCommentCommand {
        site_id: SiteId::from("my-blog"),
        post_slug: PostSlug::from("hello-world"),
        content: "hello".to_string(),
        media: None,
        location: None,
        display_name: "Alice".to_string(),
        author_public_key: "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string(),
        author_signature: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
        author_challenge: "1728000000.deadbeef.sig".to_string(),
        reply_to: None,
    }
}

fn update_command() -> UpdateCommentCommand {
    UpdateCommentCommand {
        site_id: SiteId::from("my-blog"),
        post_slug: PostSlug::from("hello-world"),
        event_id: "$original:hs".to_string(),
        content: "edited".to_string(),
        author_public_key: "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string(),
        author_signature: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
        author_challenge: "1728000000.deadbeef.sig".to_string(),
    }
}

fn delete_command() -> DeleteCommentCommand {
    DeleteCommentCommand {
        site_id: SiteId::from("my-blog"),
        post_slug: PostSlug::from("hello-world"),
        event_id: "$original:hs".to_string(),
        author_public_key: "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string(),
        author_signature: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
        author_challenge: "1728000000.deadbeef.sig".to_string(),
    }
}

/// Unique SQLite file per test to avoid shared in-memory state.
fn test_db_url(name: &str) -> String {
    // Use /tmp directly: some environments (e.g. devenv) set an empty TMPDIR,
    // which makes std::env::temp_dir() return an unusable empty path.
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-test-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn claim_holds_a_lease() {
    let store = DbStore::connect(&test_db_url("lease"))
        .await
        .expect("connect db");
    store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");

    // A held lease excludes the row from the pending view and from further
    // claims, and recovery does not touch it while it is still valid.
    let claimed = store
        .claim_pending_post_submissions(100, lease(Duration::minutes(5)))
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    assert!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending")
            .is_empty(),
        "claimed rows are not pending"
    );
    assert!(
        store
            .claim_pending_post_submissions(100, lease(Duration::minutes(5)))
            .await
            .expect("second claim")
            .is_empty(),
        "a held lease cannot be claimed twice"
    );
    let recovered = store
        .recover_expired_submission_leases()
        .await
        .expect("recover while held");
    assert_eq!(recovered, 0, "a valid lease must not be recovered");
}

#[tokio::test]
async fn expired_lease_is_recovered() {
    let store = DbStore::connect(&test_db_url("lease-expired"))
        .await
        .expect("connect db");
    store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");

    // Claiming with an already-expired lease models a crashed reconciler.
    let claimed = store
        .claim_pending_post_submissions(100, Utc::now() - Duration::seconds(1))
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    let recovered = store
        .recover_expired_submission_leases()
        .await
        .expect("recover");
    assert_eq!(recovered, 1);
    assert_eq!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending after recovery")
            .len(),
        1
    );
}

#[tokio::test]
async fn waiting_for_sync_timeout_query_and_dead_letter() {
    let store = DbStore::connect(&test_db_url("timeout"))
        .await
        .expect("connect in-memory db");

    store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");
    let pending = store
        .get_pending_post_submissions(100)
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1);
    let id = pending[0].id;

    store
        .mark_post_submission_waiting_for_sync(id, "$event:hs", "!room:hs")
        .await
        .expect("mark waiting");

    // Not stuck yet when the cutoff predates the write.
    let fresh = store
        .get_stuck_post_submissions(Utc::now() - Duration::minutes(10), 100)
        .await
        .expect("fresh query");
    assert!(fresh.is_empty());

    // Stuck once the cutoff passes the write time, with event_id/room_id.
    let stuck = store
        .get_stuck_post_submissions(Utc::now() + Duration::minutes(1), 100)
        .await
        .expect("stuck query");
    assert_eq!(stuck.len(), 1);
    assert_eq!(stuck[0].id, id);
    assert_eq!(stuck[0].event_id, "$event:hs");
    assert_eq!(stuck[0].room_id.as_deref(), Some("!room:hs"));

    // Dead-lettering removes it from both stuck and pending views.
    store
        .dead_letter_post_submission(id, "event exists but never projected")
        .await
        .expect("dead letter");
    let stuck = store
        .get_stuck_post_submissions(Utc::now() + Duration::minutes(1), 100)
        .await
        .expect("stuck after dead letter");
    assert!(stuck.is_empty());
    assert!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending after dead letter")
            .is_empty()
    );
}

#[tokio::test]
async fn pending_submission_batch_is_limited() {
    let store = DbStore::connect(&test_db_url("batch-limit"))
        .await
        .expect("connect db");

    for _ in 0..3 {
        store
            .save_post_submission(&post_command())
            .await
            .expect("save submission");
    }

    let batch = store
        .get_pending_post_submissions(2)
        .await
        .expect("limited batch");
    assert_eq!(batch.len(), 2);

    let all = store
        .get_pending_post_submissions(100)
        .await
        .expect("full batch");
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn post_txn_id_is_persisted_before_claim_and_reused() {
    let store = DbStore::connect(&test_db_url("post-txn-id"))
        .await
        .expect("connect db");
    let id = store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");
    store
        .set_post_submission_txn_id(id, "cumments_post_<random>")
        .await
        .expect("persist txn id");

    let claimed = store
        .claim_pending_post_submissions(100, lease(Duration::minutes(5)))
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(
        claimed[0].txn_id.as_deref(),
        Some("cumments_post_<random>"),
        "the allocated txn id must be persisted so retries reuse it"
    );
}

#[tokio::test]
async fn cleared_post_txn_id_is_claimed_as_none() {
    let store = DbStore::connect(&test_db_url("clear-txn"))
        .await
        .expect("connect db");
    let id = store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");
    store
        .set_post_submission_txn_id(id, "old-txn")
        .await
        .expect("persist old txn id");
    store
        .clear_post_submission_txn_id(id)
        .await
        .expect("clear txn id");

    let claimed = store
        .claim_pending_post_submissions(100, lease(Duration::minutes(5)))
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    assert!(
        claimed[0].txn_id.is_none(),
        "the stale txn id must be cleared so the next attempt allocates a fresh one"
    );

    store
        .mark_post_submission_waiting_for_sync(id, "$event:hs", "!room:hs")
        .await
        .expect("mark waiting for sync");
}

#[tokio::test]
async fn delete_txn_id_is_persisted_before_claim_and_reused() {
    let store = DbStore::connect(&test_db_url("delete-txn-id"))
        .await
        .expect("connect db");
    let id = store
        .save_delete_submission(&delete_command())
        .await
        .expect("save delete submission");
    store
        .set_delete_submission_txn_id(id, "cumments_delete_<random>")
        .await
        .expect("persist txn id");

    let claimed = store
        .claim_pending_delete_submissions(100, lease(Duration::minutes(5)))
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(
        claimed[0].txn_id.as_deref(),
        Some("cumments_delete_<random>"),
        "delete retries must reuse the persisted txn id"
    );
}

#[tokio::test]
async fn update_txn_id_is_persisted_before_claim_and_reused() {
    let store = DbStore::connect(&test_db_url("update-txn-id"))
        .await
        .expect("connect db");
    let id = store
        .save_update_submission(&update_command())
        .await
        .expect("save update submission");
    store
        .set_update_submission_txn_id(id, "cumments_update_<random>")
        .await
        .expect("persist txn id");

    let claimed = store
        .claim_pending_update_submissions(100, lease(Duration::minutes(5)))
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(
        claimed[0].txn_id.as_deref(),
        Some("cumments_update_<random>"),
        "update retries must reuse the persisted txn id"
    );
}

#[tokio::test]
async fn failure_records_schedule_retry_then_dead_letters() {
    let store = DbStore::connect(&test_db_url("retry"))
        .await
        .expect("connect in-memory db");

    store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");
    let pending = store
        .get_pending_post_submissions(100)
        .await
        .expect("pending");
    let id = pending[0].id;

    // First failure: retried (back to pending, but not due immediately).
    let retrying = store
        .record_post_submission_failure(id, "hs unreachable")
        .await
        .expect("record failure");
    assert!(retrying);

    let due_now = store
        .get_pending_post_submissions(100)
        .await
        .expect("pending query");
    assert!(
        due_now.is_empty(),
        "retried submission must wait out its backoff window"
    );

    // Exhaust the retry budget (4 more failures -> 5 total).
    for _ in 0..4 {
        store
            .record_post_submission_failure(id, "still failing")
            .await
            .expect("record failure");
    }
    let retrying = store
        .record_post_submission_failure(id, "last failure")
        .await
        .expect("record final failure");
    assert!(
        !retrying,
        "submission should be dead-lettered after budget exhaustion"
    );
}

#[tokio::test]
async fn update_submission_completion_closes_loop_and_never_regresses() {
    let store = DbStore::connect(&test_db_url("update-complete"))
        .await
        .expect("connect in-memory db");

    store
        .save_update_submission(&update_command())
        .await
        .expect("save update submission");
    let pending = store
        .get_pending_update_submissions(100)
        .await
        .expect("pending");
    let id = pending[0].id;

    // Simulate the projector seeing the replacement before the reconciler's
    // write-back: complete first, then attempt the write-back.
    store
        .mark_update_submission_completed_by_id(id)
        .await
        .expect("complete");
    store
        .mark_update_submission_waiting_for_sync(id, "$update:hs", "!room:hs")
        .await
        .expect("late write-back");

    let stuck = store
        .get_stuck_update_submissions(Utc::now() + Duration::minutes(1), 100)
        .await
        .expect("stuck query");
    assert!(
        stuck.is_empty(),
        "completed update submission must not be regressed to waiting_for_sync"
    );

    // A late failure must not resurrect a completed submission.
    let retrying = store
        .record_update_submission_failure(id, "late failure")
        .await
        .expect("record failure");
    assert!(!retrying, "completed submission must not be rescheduled");
    assert!(
        store
            .get_pending_update_submissions(100)
            .await
            .expect("pending query")
            .is_empty(),
        "completed submission must not reappear as pending"
    );
}

#[tokio::test]
async fn update_completion_by_event_id_only_closes_waiting_submissions() {
    let store = DbStore::connect(&test_db_url("update-complete-scope"))
        .await
        .expect("connect in-memory db");

    store
        .save_update_submission(&update_command())
        .await
        .expect("save first update");
    store
        .save_update_submission(&update_command())
        .await
        .expect("save second update");

    let pending = store
        .get_pending_update_submissions(100)
        .await
        .expect("pending");
    assert_eq!(pending.len(), 2);
    let first_id = pending[0].id;
    let second_id = pending[1].id;

    // One edit is observed after its write-back; the other is still pending.
    store
        .mark_update_submission_waiting_for_sync(first_id, "$update:hs", "!room:hs")
        .await
        .expect("mark first waiting");
    store
        .mark_update_submission_completed(
            "$original:hs",
            Some("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc"),
        )
        .await
        .expect("complete observed edit");

    let pending = store
        .get_pending_update_submissions(100)
        .await
        .expect("pending");
    assert_eq!(
        pending.len(),
        1,
        "pending edit must not be closed by another edit"
    );
    assert_eq!(pending[0].id, second_id);

    let stuck = store
        .get_stuck_update_submissions(Utc::now() + Duration::minutes(1), 100)
        .await
        .expect("stuck query");
    assert!(stuck.is_empty(), "observed edit must not remain waiting");
}

#[tokio::test]
async fn timeout_confirmations_increment_and_reset() {
    let store = DbStore::connect(&test_db_url("timeout-confirmations"))
        .await
        .expect("connect db");

    store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");
    let pending = store
        .get_pending_post_submissions(100)
        .await
        .expect("pending");
    let id = pending[0].id;
    store
        .mark_post_submission_waiting_for_sync(id, "$event:hs", "!room:hs")
        .await
        .expect("mark waiting");

    assert_eq!(
        store
            .increment_post_timeout_confirmation(id)
            .await
            .expect("increment"),
        1
    );
    assert_eq!(
        store
            .increment_post_timeout_confirmation(id)
            .await
            .expect("increment again"),
        2
    );
    store
        .reset_post_timeout_confirmations(id)
        .await
        .expect("reset");
    assert_eq!(
        store
            .increment_post_timeout_confirmation(id)
            .await
            .expect("increment after reset"),
        1
    );
}

#[tokio::test]
async fn timeout_confirmation_enforces_cooldown_between_passes() {
    let store = DbStore::connect(&test_db_url("timeout-cooldown"))
        .await
        .expect("connect db");

    store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");
    let pending = store
        .get_pending_post_submissions(100)
        .await
        .expect("pending");
    let id = pending[0].id;
    store
        .mark_post_submission_waiting_for_sync(id, "$event:hs", "!room:hs")
        .await
        .expect("mark waiting");

    // A future cutoff makes the submission eligible by age; the first
    // confirmation must still put it into the cooldown window so the same
    // reconcile loop cannot select it again immediately.
    store
        .increment_post_timeout_confirmation(id)
        .await
        .expect("increment");
    let stuck = store
        .get_stuck_post_submissions(Utc::now() + Duration::minutes(1), 100)
        .await
        .expect("stuck query");
    assert!(
        stuck.is_empty(),
        "confirmed submission must wait for the next confirmation cooldown"
    );
}

#[tokio::test]
async fn timeout_check_errors_increment_and_reset() {
    let store = DbStore::connect(&test_db_url("timeout-errors"))
        .await
        .expect("connect db");

    store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");
    let pending = store
        .get_pending_post_submissions(100)
        .await
        .expect("pending");
    let id = pending[0].id;
    store
        .mark_post_submission_waiting_for_sync(id, "$event:hs", "!room:hs")
        .await
        .expect("mark waiting");

    assert_eq!(
        store
            .increment_post_timeout_error(id)
            .await
            .expect("increment"),
        1
    );
    assert_eq!(
        store
            .increment_post_timeout_error(id)
            .await
            .expect("increment again"),
        2
    );
    store.reset_post_timeout_errors(id).await.expect("reset");
    assert_eq!(
        store
            .increment_post_timeout_error(id)
            .await
            .expect("increment after reset"),
        1
    );
}

#[tokio::test]
async fn failed_post_submission_can_complete_when_event_is_observed() {
    let store = DbStore::connect(&test_db_url("failed-complete"))
        .await
        .expect("connect db");

    store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");
    let pending = store
        .get_pending_post_submissions(100)
        .await
        .expect("pending");
    let id = pending[0].id;
    store
        .mark_post_submission_waiting_for_sync(id, "$event:hs", "!room:hs")
        .await
        .expect("mark waiting");
    store
        .dead_letter_post_submission(id, "event exists but never projected")
        .await
        .expect("dead letter");

    // The projector later observes the event (push arrived after the timeout
    // pass); a failed submission may now transition to completed.
    store
        .mark_post_submission_completed_by_id(id)
        .await
        .expect("complete failed submission");

    let pending = store
        .get_pending_post_submissions(100)
        .await
        .expect("pending");
    let stuck = store
        .get_stuck_post_submissions(Utc::now() + Duration::minutes(1), 100)
        .await
        .expect("stuck");
    assert!(pending.is_empty());
    assert!(stuck.is_empty());
}

#[tokio::test]
async fn failure_records_do_not_resurrect_failed_submissions() {
    let store = DbStore::connect(&test_db_url("no-resurrect"))
        .await
        .expect("connect in-memory db");

    store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");
    let pending = store
        .get_pending_post_submissions(100)
        .await
        .expect("pending");
    let id = pending[0].id;

    // Dead-letter directly (retry_count stays below the budget).
    store
        .dead_letter_post_submission(id, "event exists but never projected")
        .await
        .expect("dead letter");

    let retrying = store
        .record_post_submission_failure(id, "late failure")
        .await
        .expect("record failure");
    assert!(
        !retrying,
        "dead-lettered submission must not be resurrected by a late failure"
    );
    assert!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending query")
            .is_empty()
    );
}
