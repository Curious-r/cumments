use chrono::{Duration, Utc};
use cumments_core::{
    intents::{PostCommentIntent, UpdateCommentIntent},
    models::{PostSlug, SiteId},
    ports::IntentStore,
};
use cumments_store::DbStore;

fn post_intent() -> PostCommentIntent {
    PostCommentIntent {
        site_id: SiteId::from("my-blog"),
        post_slug: PostSlug::from("hello-world"),
        content: "hello".to_string(),
        display_name: "Alice".to_string(),
        author_public_key: "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string(),
        author_signature: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
        author_challenge: "1728000000.deadbeef.sig".to_string(),
        reply_to: None,
    }
}

fn update_intent() -> UpdateCommentIntent {
    UpdateCommentIntent {
        site_id: SiteId::from("my-blog"),
        post_slug: PostSlug::from("hello-world"),
        event_id: "$original:hs".to_string(),
        content: "edited".to_string(),
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
async fn waiting_for_sync_timeout_query_and_dead_letter() {
    let store = DbStore::connect(&test_db_url("timeout"))
        .await
        .expect("connect in-memory db");

    store
        .save_post_intent(&post_intent())
        .await
        .expect("save intent");
    let pending = store.get_pending_post_intents().await.expect("pending");
    assert_eq!(pending.len(), 1);
    let id = pending[0].id;

    store
        .mark_post_intent_waiting_for_sync(id, "$event:hs", "!room:hs")
        .await
        .expect("mark waiting");

    // Not stuck yet when the cutoff predates the write.
    let fresh = store
        .get_stuck_post_intents(Utc::now() - Duration::minutes(10))
        .await
        .expect("fresh query");
    assert!(fresh.is_empty());

    // Stuck once the cutoff passes the write time, with event_id/room_id.
    let stuck = store
        .get_stuck_post_intents(Utc::now() + Duration::minutes(1))
        .await
        .expect("stuck query");
    assert_eq!(stuck.len(), 1);
    assert_eq!(stuck[0].id, id);
    assert_eq!(stuck[0].event_id, "$event:hs");
    assert_eq!(stuck[0].room_id.as_deref(), Some("!room:hs"));

    // Dead-lettering removes it from both stuck and pending views.
    store
        .dead_letter_post_intent(id, "event exists but never projected")
        .await
        .expect("dead letter");
    let stuck = store
        .get_stuck_post_intents(Utc::now() + Duration::minutes(1))
        .await
        .expect("stuck after dead letter");
    assert!(stuck.is_empty());
    assert!(
        store
            .get_pending_post_intents()
            .await
            .expect("pending after dead letter")
            .is_empty()
    );
}

#[tokio::test]
async fn failure_records_schedule_retry_then_dead_letters() {
    let store = DbStore::connect(&test_db_url("retry"))
        .await
        .expect("connect in-memory db");

    store
        .save_post_intent(&post_intent())
        .await
        .expect("save intent");
    let pending = store.get_pending_post_intents().await.expect("pending");
    let id = pending[0].id;

    // First failure: retried (back to pending, but not due immediately).
    let retrying = store
        .record_post_intent_failure(id, "hs unreachable")
        .await
        .expect("record failure");
    assert!(retrying);

    let due_now = store
        .get_pending_post_intents()
        .await
        .expect("pending query");
    assert!(
        due_now.is_empty(),
        "retried intent must wait out its backoff window"
    );

    // Exhaust the retry budget (4 more failures -> 5 total).
    for _ in 0..4 {
        store
            .record_post_intent_failure(id, "still failing")
            .await
            .expect("record failure");
    }
    let retrying = store
        .record_post_intent_failure(id, "last failure")
        .await
        .expect("record final failure");
    assert!(
        !retrying,
        "intent should be dead-lettered after budget exhaustion"
    );
}

#[tokio::test]
async fn update_intent_completion_closes_loop_and_never_regresses() {
    let store = DbStore::connect(&test_db_url("update-complete"))
        .await
        .expect("connect in-memory db");

    store
        .save_update_intent(&update_intent())
        .await
        .expect("save update intent");
    let pending = store.get_pending_update_intents().await.expect("pending");
    let id = pending[0].id;

    // Simulate the projector seeing the replacement before the reconciler's
    // write-back: complete first, then attempt the write-back.
    store
        .mark_update_intent_completed_by_id(id)
        .await
        .expect("complete");
    store
        .mark_update_intent_waiting_for_sync(id, "!room:hs")
        .await
        .expect("late write-back");

    let stuck = store
        .get_stuck_update_intent_ids(Utc::now() + Duration::minutes(1))
        .await
        .expect("stuck query");
    assert!(
        stuck.is_empty(),
        "completed update intent must not be regressed to waiting_for_sync"
    );

    // A late failure must not resurrect a completed intent.
    let retrying = store
        .record_update_intent_failure(id, "late failure")
        .await
        .expect("record failure");
    assert!(!retrying, "completed intent must not be rescheduled");
    assert!(
        store
            .get_pending_update_intents()
            .await
            .expect("pending query")
            .is_empty(),
        "completed intent must not reappear as pending"
    );
}

#[tokio::test]
async fn update_completion_by_event_id_only_closes_waiting_intents() {
    let store = DbStore::connect(&test_db_url("update-complete-scope"))
        .await
        .expect("connect in-memory db");

    store
        .save_update_intent(&update_intent())
        .await
        .expect("save first update");
    store
        .save_update_intent(&update_intent())
        .await
        .expect("save second update");

    let pending = store.get_pending_update_intents().await.expect("pending");
    assert_eq!(pending.len(), 2);
    let first_id = pending[0].id;
    let second_id = pending[1].id;

    // One edit is observed after its write-back; the other is still pending.
    store
        .mark_update_intent_waiting_for_sync(first_id, "!room:hs")
        .await
        .expect("mark first waiting");
    store
        .mark_update_intent_completed("$original:hs")
        .await
        .expect("complete observed edit");

    let pending = store.get_pending_update_intents().await.expect("pending");
    assert_eq!(
        pending.len(),
        1,
        "pending edit must not be closed by another edit"
    );
    assert_eq!(pending[0].id, second_id);

    let stuck = store
        .get_stuck_update_intent_ids(Utc::now() + Duration::minutes(1))
        .await
        .expect("stuck query");
    assert!(stuck.is_empty(), "observed edit must not remain waiting");
}

#[tokio::test]
async fn failure_records_do_not_resurrect_failed_intents() {
    let store = DbStore::connect(&test_db_url("no-resurrect"))
        .await
        .expect("connect in-memory db");

    store
        .save_post_intent(&post_intent())
        .await
        .expect("save intent");
    let pending = store.get_pending_post_intents().await.expect("pending");
    let id = pending[0].id;

    // Dead-letter directly (retry_count stays below the budget).
    store
        .dead_letter_post_intent(id, "event exists but never projected")
        .await
        .expect("dead letter");

    let retrying = store
        .record_post_intent_failure(id, "late failure")
        .await
        .expect("record failure");
    assert!(
        !retrying,
        "dead-lettered intent must not be resurrected by a late failure"
    );
    assert!(
        store
            .get_pending_post_intents()
            .await
            .expect("pending query")
            .is_empty()
    );
}
