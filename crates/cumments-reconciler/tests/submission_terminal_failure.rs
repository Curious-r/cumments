//! Deterministic Matrix failures terminate a durable submission instead of
//! retrying it.
//!
//! A homeserver that classifies a room event as too large (`M_TOO_LARGE`) will
//! do so again for the same event, so the post/update/delete pass fails the
//! submission immediately: no backoff, no second send, no room retirement.
//! Ordinary transient failures and `RoomGone` keep their existing semantics.

use std::sync::Arc;
use std::time::Duration;

use cumments_core::commands::{DeleteCommentCommand, PostCommentCommand, UpdateCommentCommand};
use cumments_core::matrix_error::MatrixError;
use cumments_core::models::{PageSlug, SiteId};
use cumments_core::ports::{RegistryStore, SiteStore, SubmissionStore};
use cumments_core::site_service::SiteService;
use cumments_reconciler::{
    DeletionsPass, PassConfig, PostsPass, ReconcilePass, ReconcilerDeps, UpdatesPass,
};
use cumments_store::DbStore;
use cumments_test_utils::TestDriver;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-terminal-failure-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

fn pass_config(name: &'static str) -> PassConfig {
    PassConfig {
        name,
        interval: Duration::from_secs(5),
        wakeup: Arc::new(tokio::sync::Notify::new()),
    }
}

/// A store with a provisioned site and a registered room, so the pass reaches
/// the Matrix send instead of the room-provisioning steps.
async fn prepare(name: &str) -> (Arc<DbStore>, Arc<TestDriver>) {
    let store = Arc::new(
        DbStore::connect(&test_db_url(name))
            .await
            .expect("connect db"),
    );
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("provision site");
    store
        .register_room(
            "!room:hs",
            &SiteId::from("my-blog"),
            &PageSlug::from("hello-world"),
        )
        .await
        .expect("register room");
    (store, Arc::new(TestDriver::new()))
}

fn reconciler_deps(store: &Arc<DbStore>, driver: &Arc<TestDriver>) -> Arc<ReconcilerDeps> {
    let state_redaction_repairer: Arc<dyn cumments_core::ports::StateRedactionRepairer> =
        driver.clone();
    Arc::new(ReconcilerDeps {
        submission_store: store.clone(),
        registry_store: store.clone(),
        site_store: store.clone(),
        role_claim_store: store.clone(),
        governance_store: store.clone(),
        projection_repair_store: store.clone(),
        message_store: store.clone(),
        room_store: store.clone(),
        virtual_user_store: store.clone(),
        site_auth_store: store.clone(),
        site_transfer_store: store.clone(),
        state_redaction_repairer,
        driver: driver.clone(),
        site_service: Arc::new(SiteService::new(store.clone())),
        profile_store: Some(store.clone()),
    })
}

/// The stored status and diagnostic of a submission row.
async fn stored_status(store: &DbStore, table: &str, id: i64) -> (String, Option<String>) {
    use sea_orm::ConnectionTrait;

    let statement = sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Sqlite,
        format!("SELECT status, last_error FROM {table} WHERE id = {id}"),
    );
    let row = store
        .connection()
        .query_one_raw(statement)
        .await
        .expect("query submission row")
        .expect("submission row exists");
    (
        row.try_get("", "status").expect("status column"),
        row.try_get("", "last_error").expect("last_error column"),
    )
}

fn too_large() -> anyhow::Error {
    anyhow::Error::new(MatrixError::RequestTooLarge {
        context: "M_TOO_LARGE: event is too large".to_string(),
    })
}

fn post_command() -> PostCommentCommand {
    PostCommentCommand {
        site_id: SiteId::from("my-blog"),
        page_slug: PageSlug::from("hello-world"),
        content: "hello".to_string(),
        media: None,
        location: None,
        poll: None,
        author_public_key: "pubkey".to_string(),
        author_signature: "sig".to_string(),
        author_challenge: "chal".to_string(),
        reply_to: None,
        thread_root: None,
    }
}

fn update_command() -> UpdateCommentCommand {
    UpdateCommentCommand {
        site_id: SiteId::from("my-blog"),
        page_slug: PageSlug::from("hello-world"),
        event_id: "$original:hs".to_string(),
        content: "edited".to_string(),
        author_public_key: "pubkey".to_string(),
        author_signature: "sig".to_string(),
        author_challenge: "chal".to_string(),
    }
}

fn delete_command() -> DeleteCommentCommand {
    DeleteCommentCommand {
        site_id: SiteId::from("my-blog"),
        page_slug: PageSlug::from("hello-world"),
        event_id: "$original:hs".to_string(),
        author_public_key: "pubkey".to_string(),
        author_signature: "sig".to_string(),
        author_challenge: "chal".to_string(),
    }
}

#[tokio::test]
async fn too_large_post_submission_fails_terminally_without_a_second_send() {
    let (store, driver) = prepare("too-large-post").await;
    driver.fail_next_post_message(too_large()).await;
    let id = store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");

    let pass = PostsPass::new(reconciler_deps(&store, &driver), pass_config("posts"));
    assert_eq!(pass.run().await.expect("reconcile"), 1);

    let (status, last_error) = stored_status(&store, "post_submissions", id).await;
    assert_eq!(status, "failed");
    assert!(
        last_error.unwrap().contains("too large"),
        "the diagnostic must be retained"
    );
    // No second attempt: the injected failure was the only send, and a retry
    // would have succeeded (and been recorded) because no failure remained.
    assert!(
        driver.post_message_failures.lock().await.is_empty(),
        "the queued failure must have been consumed exactly once"
    );
    assert!(
        driver.posted_messages.lock().await.is_empty(),
        "a terminally failed event must never be re-sent"
    );
    assert_eq!(pass.run().await.expect("second pass"), 0);
}

#[tokio::test]
async fn too_large_update_submission_fails_terminally() {
    let (store, driver) = prepare("too-large-update").await;
    driver.fail_next_update_message(too_large()).await;
    let id = store
        .save_update_submission(&update_command())
        .await
        .expect("save submission");

    let pass = UpdatesPass::new(reconciler_deps(&store, &driver), pass_config("updates"));
    assert_eq!(pass.run().await.expect("reconcile"), 1);

    let (status, last_error) = stored_status(&store, "update_submissions", id).await;
    assert_eq!(status, "failed");
    assert!(last_error.unwrap().contains("too large"));
    assert!(driver.updated_messages.lock().await.is_empty());
    assert_eq!(pass.run().await.expect("second pass"), 0);
}

#[tokio::test]
async fn too_large_delete_submission_fails_terminally() {
    let (store, driver) = prepare("too-large-delete").await;
    driver.fail_next_redact_message(too_large()).await;
    let id = store
        .save_delete_submission(&delete_command())
        .await
        .expect("save submission");

    let pass = DeletionsPass::new(reconciler_deps(&store, &driver), pass_config("deletions"));
    assert_eq!(pass.run().await.expect("reconcile"), 1);

    let (status, last_error) = stored_status(&store, "delete_submissions", id).await;
    assert_eq!(status, "failed");
    assert!(last_error.unwrap().contains("too large"));
    assert!(driver.redactions.lock().await.is_empty());
    assert_eq!(pass.run().await.expect("second pass"), 0);
}

#[tokio::test]
async fn transient_failures_still_retry() {
    let (store, driver) = prepare("transient-post").await;
    driver
        .fail_next_post_message(anyhow::anyhow!("homeserver unreachable"))
        .await;
    let id = store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");

    let pass = PostsPass::new(reconciler_deps(&store, &driver), pass_config("posts"));
    assert_eq!(pass.run().await.expect("reconcile"), 1);

    let (status, last_error) = stored_status(&store, "post_submissions", id).await;
    assert_eq!(status, "pending", "an ordinary failure is retried");
    assert!(last_error.unwrap().contains("unreachable"));

    // The retry succeeds once the homeserver recovers.
    let processed = pass.run().await.expect("retry pass");
    assert_eq!(processed, 0, "the retry waits out its backoff window");
    assert!(driver.posted_messages.lock().await.is_empty());
}

#[tokio::test]
async fn room_gone_retires_the_registry_entry_and_retries() {
    let (store, driver) = prepare("room-gone").await;
    driver
        .fail_next_post_message(anyhow::Error::new(MatrixError::RoomGone {
            room_id: "!room:hs".to_string(),
            reason: "tombstoned".to_string(),
        }))
        .await;
    let id = store
        .save_post_submission(&post_command())
        .await
        .expect("save submission");

    let pass = PostsPass::new(reconciler_deps(&store, &driver), pass_config("posts"));
    assert_eq!(pass.run().await.expect("reconcile"), 1);

    let (status, _) = stored_status(&store, "post_submissions", id).await;
    assert_eq!(status, "pending", "RoomGone keeps its retry semantics");
    assert!(
        store
            .get_registered_room(&SiteId::from("my-blog"), &PageSlug::from("hello-world"))
            .await
            .expect("registry lookup")
            .is_none(),
        "the registry entry is retired so the retry can adopt a successor"
    );
}
