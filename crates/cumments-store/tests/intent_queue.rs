use chrono::{Duration, Utc};
use cumments_core::{
    intents::PostCommentIntent,
    models::{PostSlug, SiteId},
    ports::IntentStore,
};
use cumments_store::DbStore;

fn post_intent() -> PostCommentIntent {
    PostCommentIntent {
        site_id: SiteId::from("my-blog"),
        post_slug: PostSlug::from("hello-world"),
        content: "hello".to_string(),
        nickname: "Alice".to_string(),
        email: None,
        author_public_key: "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string(),
        author_signature: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
        reply_to: None,
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
    let (id, _) = pending[0];

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
    assert_eq!(stuck[0].0, id);
    assert_eq!(stuck[0].1, "$event:hs");
    assert_eq!(stuck[0].2.as_deref(), Some("!room:hs"));

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
    let (id, _) = pending[0];

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
