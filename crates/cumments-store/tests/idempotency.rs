use chrono::{Duration, Utc};
use cumments_core::{
    commands::PostCommentCommand,
    models::{PostSlug, SiteId},
    ports::{IdempotencyInput, IdempotencyOutcome, SubmissionStore},
};
use cumments_store::{DbStore, entities::idempotency_keys};
use sea_orm::{Database, EntityTrait, Set};

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

fn input(key: &str, fingerprint: &str) -> IdempotencyInput {
    IdempotencyInput {
        author_public_key: "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string(),
        key: key.to_string(),
        request_fingerprint: fingerprint.to_string(),
    }
}

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-idempotency-test-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn same_key_and_fingerprint_replays_original_submission() {
    let store = DbStore::connect(&test_db_url("replay"))
        .await
        .expect("connect db");

    let first = store
        .save_post_submission_idempotent(&post_command(), &input("retry-me-123", "fingerprint-a"))
        .await
        .expect("first submit");
    let IdempotencyOutcome::Accepted { submission_id } = first else {
        panic!("first submit must be accepted, got {first:?}");
    };

    let replay = store
        .save_post_submission_idempotent(&post_command(), &input("retry-me-123", "fingerprint-a"))
        .await
        .expect("retry submit");
    assert_eq!(
        replay,
        IdempotencyOutcome::Replayed { submission_id },
        "identical retry must return the original submission"
    );

    // Only one submission was queued.
    let pending = store
        .get_pending_post_submissions(100)
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, submission_id);
}

#[tokio::test]
async fn same_key_with_different_fingerprint_is_reused() {
    let store = DbStore::connect(&test_db_url("reused"))
        .await
        .expect("connect db");

    let first = store
        .save_post_submission_idempotent(&post_command(), &input("retry-me-123", "fingerprint-a"))
        .await
        .expect("first submit");
    assert!(matches!(first, IdempotencyOutcome::Accepted { .. }));

    let reused = store
        .save_post_submission_idempotent(&post_command(), &input("retry-me-123", "fingerprint-b"))
        .await
        .expect("second submit");
    assert_eq!(reused, IdempotencyOutcome::Reused);

    // The rejected request must not have queued a submission.
    let pending = store
        .get_pending_post_submissions(100)
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1);
}

#[tokio::test]
async fn expired_keys_are_purged_and_can_be_reused() {
    let url = test_db_url("expiry");
    let store = DbStore::connect(&url).await.expect("connect store");
    let db = Database::connect(&url).await.expect("connect raw db");

    // Simulate a record from >24h ago by writing it directly.
    idempotency_keys::Entity::insert(idempotency_keys::ActiveModel {
        author_public_key: Set("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string()),
        idempotency_key: Set("stale-key-123".to_string()),
        request_fingerprint: Set("fingerprint-stale".to_string()),
        submission_id: Set(999),
        created_at: Set(Utc::now() - Duration::hours(25)),
        ..Default::default()
    })
    .exec(&db)
    .await
    .expect("insert stale row");

    let outcome = store
        .save_post_submission_idempotent(
            &post_command(),
            &input("stale-key-123", "fingerprint-new"),
        )
        .await
        .expect("submit with expired key");
    assert!(
        matches!(outcome, IdempotencyOutcome::Accepted { .. }),
        "expired key must be treated as unused, got {outcome:?}"
    );

    // The stale row is gone and exactly one submission is queued.
    let rows = idempotency_keys::Entity::find()
        .all(&db)
        .await
        .expect("list idempotency rows");
    assert_eq!(rows.len(), 1);
    assert_ne!(
        rows[0].submission_id, 999,
        "stale row must be purged before the new record is written"
    );
}

#[tokio::test]
async fn same_key_from_different_authors_is_independent() {
    let store = DbStore::connect(&test_db_url("authors"))
        .await
        .expect("connect db");

    let mut other = post_command();
    other.author_public_key = "DwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string();
    let mut other_input = input("shared-key-123", "fingerprint-a");
    other_input.author_public_key = other.author_public_key.clone();

    let first = store
        .save_post_submission_idempotent(&post_command(), &input("shared-key-123", "fingerprint-a"))
        .await
        .expect("author A submit");
    let second = store
        .save_post_submission_idempotent(&other, &other_input)
        .await
        .expect("author B submit");

    assert!(matches!(first, IdempotencyOutcome::Accepted { .. }));
    assert!(
        matches!(second, IdempotencyOutcome::Accepted { .. }),
        "the same key must be scoped per author, got {second:?}"
    );
}

#[tokio::test]
async fn concurrent_identical_submissions_queue_only_one_submission() {
    let store = DbStore::connect(&test_db_url("concurrent"))
        .await
        .expect("connect db");

    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            store
                .save_post_submission_idempotent(
                    &post_command(),
                    &input("concurrent-key-1", "fingerprint-a"),
                )
                .await
                .expect("concurrent submit")
        }));
    }

    let mut accepted = Vec::new();
    let mut replayed = Vec::new();
    for handle in handles {
        match handle.await.expect("join task") {
            IdempotencyOutcome::Accepted { submission_id } => accepted.push(submission_id),
            IdempotencyOutcome::Replayed { submission_id } => replayed.push(submission_id),
            IdempotencyOutcome::Reused => {
                panic!("identical concurrent requests must never be reported as reused")
            }
        }
    }

    assert_eq!(accepted.len(), 1, "exactly one submit wins");
    assert_eq!(replayed.len(), 15);
    assert!(
        replayed.iter().all(|id| *id == accepted[0]),
        "all replays must return the winner.s submission"
    );

    let pending = store
        .get_pending_post_submissions(100)
        .await
        .expect("pending");
    assert_eq!(pending.len(), 1, "duplicate submissions must not be queued");
    assert_eq!(pending[0].id, accepted[0]);
}
