use chrono::{Duration, Utc};
use cumments_core::{
    commands::PostCommentCommand,
    models::{PageSlug, SiteId},
    ports::SubmissionStore,
    submissions::{IdempotencyInput, IdempotencyOutcome, OperationClaimOutcome, OperationIdentity},
};
use cumments_store::{DbStore, entities::idempotency_keys};
use sea_orm::{Database, EntityTrait, Set};

fn post_command() -> PostCommentCommand {
    PostCommentCommand {
        site_id: SiteId::from("my-blog"),
        page_slug: PageSlug::from("hello-world"),
        content: "hello".to_string(),
        media: None,
        location: None,
        poll: None,
        display_name: "Alice".to_string(),
        author_public_key: "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc".to_string(),
        author_signature: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
        author_challenge: "1728000000.deadbeef.sig".to_string(),
        reply_to: None,
        thread_root: None,
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

// ── Server-wide operation identity ────────────────────────────────

fn operation(key: &str, author: &str, fingerprint: &str) -> OperationIdentity {
    OperationIdentity::new(key, author, fingerprint)
}

fn poll_command(author: &str) -> PostCommentCommand {
    let mut command = post_command();
    command.author_public_key = author.to_string();
    command
}

#[tokio::test]
async fn operation_claim_is_server_wide_and_returns_new_replay_conflict() {
    let store = DbStore::connect(&test_db_url("op-claim"))
        .await
        .expect("connect db");

    // Unused key.
    assert_eq!(
        store
            .lookup_operation("op-key-123456")
            .await
            .expect("lookup"),
        None
    );

    // New.
    let first = store
        .save_post_submission_claimed(
            &poll_command("author-a"),
            &operation("op-key-123456", "author-a", "fp-a"),
        )
        .await
        .expect("first claim");
    let OperationClaimOutcome::Accepted { submission_id } = first else {
        panic!("first claim must be accepted, got {first:?}");
    };

    // Same author + same fingerprint -> replay of the same operation.
    let replay = store
        .save_post_submission_claimed(
            &poll_command("author-a"),
            &operation("op-key-123456", "author-a", "fp-a"),
        )
        .await
        .expect("replay");
    assert_eq!(
        replay,
        OperationClaimOutcome::Replayed { submission_id },
        "matching author+fingerprint must replay the original operation"
    );

    // Same author + different fingerprint -> conflict.
    let conflict = store
        .save_post_submission_claimed(
            &poll_command("author-a"),
            &operation("op-key-123456", "author-a", "fp-b"),
        )
        .await
        .expect("conflict");
    assert_eq!(conflict, OperationClaimOutcome::Conflict);

    // Different author (even with the same fingerprint) -> conflict.
    let cross_author = store
        .save_post_submission_claimed(
            &poll_command("author-b"),
            &operation("op-key-123456", "author-b", "fp-a"),
        )
        .await
        .expect("cross-author");
    assert_eq!(cross_author, OperationClaimOutcome::Conflict);

    // Exactly one logical operation, and only its one durable submission.
    let claim = store
        .lookup_operation("op-key-123456")
        .await
        .expect("lookup")
        .expect("claim exists");
    assert_eq!(claim.author_public_key, "author-a");
    assert_eq!(claim.fingerprint, "fp-a");
    assert_eq!(claim.submission_id, submission_id);
    assert_eq!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending")
            .len(),
        1,
        "replay/conflict must not queue additional submissions"
    );
}

#[tokio::test]
async fn operation_id_is_opaque_and_not_normalized() {
    let store = DbStore::connect(&test_db_url("op-opaque"))
        .await
        .expect("connect db");

    for key in ["Op-Key-123456", "op-key-123456", "op_key_123456"] {
        let outcome = store
            .save_post_submission_claimed(
                &poll_command("author-a"),
                &operation(key, "author-a", "fp"),
            )
            .await
            .expect("claim");
        assert!(
            matches!(outcome, OperationClaimOutcome::Accepted { .. }),
            "{key} must be treated as a distinct opaque id"
        );
    }
    assert_eq!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending")
            .len(),
        3
    );
}

#[tokio::test]
async fn operation_claim_survives_reconnect() {
    let url = test_db_url("op-persist");
    let submission_id = {
        let store = DbStore::connect(&url).await.expect("connect db");
        let OperationClaimOutcome::Accepted { submission_id } = store
            .save_post_submission_claimed(
                &poll_command("author-a"),
                &operation("op-persist-123456", "author-a", "fp"),
            )
            .await
            .expect("claim")
        else {
            panic!("must be accepted");
        };
        submission_id
    };

    // A fresh connection to the same database sees the claim.
    let store = DbStore::connect(&url).await.expect("reconnect");
    let claim = store
        .lookup_operation("op-persist-123456")
        .await
        .expect("lookup")
        .expect("claim persisted");
    assert_eq!(claim.submission_id, submission_id);
    // And a duplicate claim still replays rather than creating a second row.
    assert_eq!(
        store
            .save_post_submission_claimed(
                &poll_command("author-a"),
                &operation("op-persist-123456", "author-a", "fp"),
            )
            .await
            .expect("re-claim"),
        OperationClaimOutcome::Replayed { submission_id }
    );
    assert_eq!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending")
            .len(),
        1
    );
}

#[tokio::test]
async fn concurrent_identical_operation_claims_resolve_to_one() {
    let store = DbStore::connect(&test_db_url("op-concurrent-same"))
        .await
        .expect("connect db");

    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            store
                .save_post_submission_claimed(
                    &poll_command("author-a"),
                    &operation("op-race-key-1", "author-a", "fp-a"),
                )
                .await
                .expect("concurrent claim")
        }));
    }

    let mut accepted = Vec::new();
    let mut replayed = Vec::new();
    for handle in handles {
        match handle.await.expect("join") {
            OperationClaimOutcome::Accepted { submission_id } => accepted.push(submission_id),
            OperationClaimOutcome::Replayed { submission_id } => replayed.push(submission_id),
            OperationClaimOutcome::Conflict => {
                panic!("identical concurrent claims must never conflict")
            }
        }
    }
    assert_eq!(accepted.len(), 1, "exactly one claim wins");
    assert_eq!(replayed.len(), 15);
    assert!(
        replayed.iter().all(|id| *id == accepted[0]),
        "every replay must return the winner's submission"
    );
    assert_eq!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending")
            .len(),
        1,
        "concurrent identical claims must queue one submission"
    );
}

#[tokio::test]
async fn concurrent_conflicting_operation_claims_resolve_to_one_operation() {
    let store = DbStore::connect(&test_db_url("op-concurrent-conflict"))
        .await
        .expect("connect db");

    // Same key: one author+fingerprint pair races another. Exactly one claim
    // must win; the winner's own duplicates replay, and every attempt from the
    // other author/fingerprint conflicts — never a second operation.
    let mut handles = Vec::new();
    for _ in 0..8 {
        for (author, fingerprint) in [("author-a", "fp-a"), ("author-b", "fp-b")] {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .save_post_submission_claimed(
                        &poll_command(author),
                        &operation("op-race-key-2", author, fingerprint),
                    )
                    .await
                    .expect("concurrent claim")
            }));
        }
    }

    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(handle.await.expect("join"));
    }
    let accepted: Vec<i64> = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            OperationClaimOutcome::Accepted { submission_id } => Some(*submission_id),
            _ => None,
        })
        .collect();
    let replayed = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            OperationClaimOutcome::Replayed { submission_id } => Some(*submission_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    let conflicts = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, OperationClaimOutcome::Conflict))
        .count();

    assert_eq!(accepted.len(), 1, "only one operation may be claimed");
    assert_eq!(replayed.len(), 7, "the winner's own duplicates replay");
    assert_eq!(
        conflicts, 8,
        "the other author/fingerprint always conflicts"
    );
    assert!(
        replayed.iter().all(|id| *id == accepted[0]),
        "replays must return the winning operation"
    );
    assert_eq!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending")
            .len(),
        1,
        "only the winning operation's submission may exist"
    );
}

#[tokio::test]
async fn concurrent_same_author_different_fingerprint_resolves_deterministically() {
    let store = DbStore::connect(&test_db_url("op-concurrent-fp"))
        .await
        .expect("connect db");

    let mut handles = Vec::new();
    for _ in 0..8 {
        for fingerprint in ["fp-a", "fp-b"] {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .save_post_submission_claimed(
                        &poll_command("author-a"),
                        &operation("op-race-key-3", "author-a", fingerprint),
                    )
                    .await
                    .expect("concurrent claim")
            }));
        }
    }

    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(handle.await.expect("join"));
    }
    let accepted: Vec<i64> = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            OperationClaimOutcome::Accepted { submission_id } => Some(*submission_id),
            _ => None,
        })
        .collect();
    let conflicts = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, OperationClaimOutcome::Conflict))
        .count();
    for outcome in &outcomes {
        if let OperationClaimOutcome::Replayed { submission_id } = outcome {
            assert_eq!(
                *submission_id, accepted[0],
                "a replay must match the single winning operation"
            );
        }
    }
    assert_eq!(accepted.len(), 1, "one operation plus one fingerprint wins");
    assert_eq!(
        conflicts, 8,
        "the fingerprint that lost the race always conflicts"
    );
    assert_eq!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending")
            .len(),
        1
    );
}
