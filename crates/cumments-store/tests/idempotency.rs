use chrono::{Duration, Utc};
use cumments_core::{
    commands::PostCommentCommand,
    models::{PageSlug, SiteId},
    ports::SubmissionStore,
    submissions::{IdempotencyInput, IdempotencyOutcome, OperationClaimOutcome, OperationIdentity},
};
use cumments_store::{
    DbStore,
    entities::{idempotency_keys, operation_claims, post_submissions},
};
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

async fn claim_rows(url: &str) -> Vec<operation_claims::Model> {
    let db = Database::connect(url).await.expect("raw db");
    operation_claims::Entity::find()
        .all(&db)
        .await
        .expect("list claims")
}

#[tokio::test]
async fn operation_claim_is_independent_of_durable_submissions() {
    let url = test_db_url("op-alone");
    let store = DbStore::connect(&url).await.expect("connect db");

    assert_eq!(
        store
            .lookup_operation("op-alone-123456")
            .await
            .expect("lookup"),
        None
    );

    // New: claiming alone must not create any durable submission.
    assert_eq!(
        store
            .claim_operation(&operation("op-alone-123456", "author-a", "fp-a"))
            .await
            .expect("claim"),
        OperationClaimOutcome::New
    );
    let claim = store
        .lookup_operation("op-alone-123456")
        .await
        .expect("lookup")
        .expect("claim exists");
    assert_eq!(claim.author_public_key, "author-a");
    assert_eq!(claim.fingerprint, "fp-a");
    assert!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending")
            .is_empty(),
        "an operation claim must not require a post submission"
    );
    assert_eq!(
        claim_rows(&url).await.len(),
        1,
        "exactly one claim row exists"
    );

    // Replay: same author + same fingerprint.
    assert_eq!(
        store
            .claim_operation(&operation("op-alone-123456", "author-a", "fp-a"))
            .await
            .expect("replay"),
        OperationClaimOutcome::Replay
    );
    // Conflict: same author, different fingerprint.
    assert_eq!(
        store
            .claim_operation(&operation("op-alone-123456", "author-a", "fp-b"))
            .await
            .expect("fingerprint conflict"),
        OperationClaimOutcome::Conflict
    );
    // Conflict: different author.
    assert_eq!(
        store
            .claim_operation(&operation("op-alone-123456", "author-b", "fp-a"))
            .await
            .expect("author conflict"),
        OperationClaimOutcome::Conflict
    );

    // Still exactly one claim, still no submissions.
    assert_eq!(claim_rows(&url).await.len(), 1);
    assert!(
        store
            .get_pending_post_submissions(100)
            .await
            .expect("pending")
            .is_empty()
    );
}

#[tokio::test]
async fn releasing_an_operation_claim_requires_an_exact_identity_match() {
    let url = test_db_url("op-release");
    let store = DbStore::connect(&url).await.expect("connect db");

    assert_eq!(
        store
            .claim_operation(&operation("op-release-123456", "author-a", "fp-a"))
            .await
            .expect("claim"),
        OperationClaimOutcome::New
    );

    // A non-matching identity must never release the claim.
    store
        .release_operation(&operation("op-release-123456", "author-b", "fp-a"))
        .await
        .expect("release mismatch author");
    store
        .release_operation(&operation("op-release-123456", "author-a", "fp-b"))
        .await
        .expect("release mismatch fingerprint");
    assert_eq!(claim_rows(&url).await.len(), 1);

    // The exact identity releases it, and the operation becomes claimable anew.
    store
        .release_operation(&operation("op-release-123456", "author-a", "fp-a"))
        .await
        .expect("release");
    assert_eq!(
        store.lookup_operation("op-release-123456").await.unwrap(),
        None
    );
    assert_eq!(claim_rows(&url).await.len(), 0);

    // Releasing an absent operation is a no-op.
    store
        .release_operation(&operation("op-release-123456", "author-a", "fp-a"))
        .await
        .expect("release absent");
    assert_eq!(
        store
            .claim_operation(&operation("op-release-123456", "author-a", "fp-a"))
            .await
            .expect("reclaim"),
        OperationClaimOutcome::New
    );
}

#[tokio::test]
async fn operation_id_is_opaque_and_not_normalized() {
    let store = DbStore::connect(&test_db_url("op-opaque"))
        .await
        .expect("connect db");

    for key in ["Op-Key-123456", "op-key-123456", "op_key_123456"] {
        assert_eq!(
            store
                .claim_operation(&operation(key, "author-a", "fp"))
                .await
                .expect("claim"),
            OperationClaimOutcome::New,
            "{key} must be treated as a distinct opaque id"
        );
    }
}

#[tokio::test]
async fn operation_claim_survives_reconnect() {
    let url = test_db_url("op-persist");
    {
        let store = DbStore::connect(&url).await.expect("connect db");
        assert_eq!(
            store
                .claim_operation(&operation("op-persist-123456", "author-a", "fp"))
                .await
                .expect("claim"),
            OperationClaimOutcome::New
        );
    }

    // A fresh connection to the same database sees the claim and replays.
    let store = DbStore::connect(&url).await.expect("reconnect");
    let claim = store
        .lookup_operation("op-persist-123456")
        .await
        .expect("lookup")
        .expect("claim persisted");
    assert_eq!(claim.author_public_key, "author-a");
    assert_eq!(
        store
            .claim_operation(&operation("op-persist-123456", "author-a", "fp"))
            .await
            .expect("re-claim"),
        OperationClaimOutcome::Replay
    );
}

#[tokio::test]
async fn create_poll_claim_creates_claim_and_submission_together() {
    let url = test_db_url("op-poll");
    let store = DbStore::connect(&url).await.expect("connect db");

    let outcome = store
        .claim_post_submission(
            &poll_command("author-a"),
            &operation("op-poll-123456", "author-a", "fp-a"),
        )
        .await
        .expect("create poll claim");
    let IdempotencyOutcome::Accepted { submission_id } = outcome else {
        panic!("a new create poll operation must be accepted, got {outcome:?}");
    };

    // The operation claim exists and is independent of the submission...
    let claim = store
        .lookup_operation("op-poll-123456")
        .await
        .expect("lookup")
        .expect("claim exists");
    assert_eq!(claim.author_public_key, "author-a");
    assert_eq!(claim.fingerprint, "fp-a");
    // ...and the durable submission references the operation.
    assert_eq!(
        store
            .find_post_submission_by_operation("op-poll-123456")
            .await
            .expect("find submission"),
        Some(submission_id)
    );

    // Replay returns the original submission without queueing another.
    assert_eq!(
        store
            .claim_post_submission(
                &poll_command("author-a"),
                &operation("op-poll-123456", "author-a", "fp-a"),
            )
            .await
            .expect("replay"),
        IdempotencyOutcome::Replayed { submission_id }
    );
    // Conflicts queue nothing.
    assert_eq!(
        store
            .claim_post_submission(
                &poll_command("author-a"),
                &operation("op-poll-123456", "author-a", "fp-b"),
            )
            .await
            .expect("fingerprint conflict"),
        IdempotencyOutcome::Reused
    );
    assert_eq!(
        store
            .claim_post_submission(
                &poll_command("author-b"),
                &operation("op-poll-123456", "author-b", "fp-a"),
            )
            .await
            .expect("author conflict"),
        IdempotencyOutcome::Reused
    );

    assert_eq!(claim_rows(&url).await.len(), 1);
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
async fn comment_submissions_leave_no_operation_reference() {
    let url = test_db_url("comment-no-op");
    let store = DbStore::connect(&url).await.expect("connect db");

    store
        .save_post_submission(&post_command())
        .await
        .expect("comment submission");

    assert_eq!(
        claim_rows(&url).await.len(),
        0,
        "comments claim no operation"
    );
    let db = Database::connect(&url).await.expect("raw db");
    let rows = post_submissions::Entity::find()
        .all(&db)
        .await
        .expect("list submissions");
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].operation_id.is_none(),
        "comment submissions must not reference an operation"
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
                .claim_operation(&operation("op-race-key-1", "author-a", "fp-a"))
                .await
                .expect("concurrent claim")
        }));
    }

    let mut new = 0;
    let mut replay = 0;
    for handle in handles {
        match handle.await.expect("join") {
            OperationClaimOutcome::New => new += 1,
            OperationClaimOutcome::Replay => replay += 1,
            OperationClaimOutcome::Conflict => {
                panic!("identical concurrent claims must never conflict")
            }
        }
    }
    assert_eq!(new, 1, "exactly one claim wins");
    assert_eq!(replay, 15);
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
                    .claim_operation(&operation("op-race-key-2", author, fingerprint))
                    .await
                    .expect("concurrent claim")
            }));
        }
    }

    let mut new = 0;
    let mut replay = 0;
    let mut conflict = 0;
    for handle in handles {
        match handle.await.expect("join") {
            OperationClaimOutcome::New => new += 1,
            OperationClaimOutcome::Replay => replay += 1,
            OperationClaimOutcome::Conflict => conflict += 1,
        }
    }
    assert_eq!(new, 1, "only one operation may be claimed");
    assert_eq!(replay, 7, "the winner's own duplicates replay");
    assert_eq!(conflict, 8, "the other author/fingerprint always conflicts");
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
                    .claim_operation(&operation("op-race-key-3", "author-a", fingerprint))
                    .await
                    .expect("concurrent claim")
            }));
        }
    }

    let mut new = 0;
    let mut conflict = 0;
    for handle in handles {
        match handle.await.expect("join") {
            OperationClaimOutcome::New => new += 1,
            OperationClaimOutcome::Conflict => conflict += 1,
            OperationClaimOutcome::Replay => {}
        }
    }
    assert_eq!(new, 1, "one operation plus one fingerprint wins");
    assert_eq!(
        conflict, 8,
        "the fingerprint that lost the race always conflicts"
    );
}

#[tokio::test]
async fn concurrent_create_poll_claims_queue_one_submission() {
    let store = DbStore::connect(&test_db_url("op-concurrent-poll"))
        .await
        .expect("connect db");

    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            store
                .claim_post_submission(
                    &poll_command("author-a"),
                    &operation("op-race-poll-1", "author-a", "fp-a"),
                )
                .await
                .expect("concurrent create poll claim")
        }));
    }

    let mut accepted = Vec::new();
    let mut replayed = Vec::new();
    for handle in handles {
        match handle.await.expect("join") {
            IdempotencyOutcome::Accepted { submission_id } => accepted.push(submission_id),
            IdempotencyOutcome::Replayed { submission_id } => replayed.push(submission_id),
            IdempotencyOutcome::Reused => {
                panic!("identical concurrent create poll claims must never conflict")
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
async fn operation_execution_lifecycle_and_txn_id_persistence() {
    use cumments_core::submissions::OperationExecutionStatus;

    let url = test_db_url("op-exec-lifecycle");
    let store = DbStore::connect(&url).await.expect("connect db");

    let op_id = "op-exec-test-1";

    // Initially absent
    assert_eq!(store.get_operation_execution(op_id).await.unwrap(), None);

    // Establish execution with initial txn_id
    let exec1 = store
        .establish_operation_execution(op_id, "txn-1")
        .await
        .expect("establish");
    assert_eq!(exec1.operation_id, op_id);
    assert_eq!(exec1.txn_id, "txn-1");
    assert_eq!(exec1.status, OperationExecutionStatus::InFlight);

    // Re-establishing with a different initial_txn_id must NOT overwrite; it must return the existing one!
    let exec2 = store
        .establish_operation_execution(op_id, "txn-2-different")
        .await
        .expect("re-establish");
    assert_eq!(exec2.operation_id, op_id);
    assert_eq!(exec2.txn_id, "txn-1", "must preserve original txn_id");
    assert_eq!(exec2.status, OperationExecutionStatus::InFlight);

    // Update status to Failed
    store
        .update_operation_execution_status(op_id, OperationExecutionStatus::Failed)
        .await
        .expect("mark failed");
    let exec_failed = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("found");
    assert_eq!(exec_failed.status, OperationExecutionStatus::Failed);
    assert_eq!(exec_failed.txn_id, "txn-1");

    // Update status to InFlight (retry attempt)
    store
        .update_operation_execution_status(op_id, OperationExecutionStatus::InFlight)
        .await
        .expect("mark in_flight");
    let exec_inflight = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("found");
    assert_eq!(exec_inflight.status, OperationExecutionStatus::InFlight);

    // Update status to Success
    store
        .update_operation_execution_status(op_id, OperationExecutionStatus::Success)
        .await
        .expect("mark success");
    let exec_success = store
        .get_operation_execution(op_id)
        .await
        .unwrap()
        .expect("found");
    assert_eq!(exec_success.status, OperationExecutionStatus::Success);
    assert_eq!(exec_success.txn_id, "txn-1");
}
