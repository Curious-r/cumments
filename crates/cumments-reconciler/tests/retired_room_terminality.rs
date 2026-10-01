//! Retired comment-room instances are terminal.
//!
//! While a retired instance awaits local cleanup the page has no live room:
//! a write must not adopt the retired room, and the row must stay retired.
//! Once cleanup has released it, the same page materializes a *new* room.

use std::sync::Arc;
use std::time::Duration;

use cumments_core::commands::PostCommentCommand;
use cumments_core::models::{PageSlug, RoomStatus, SiteId};
use cumments_core::ports::{RegistryStore, SiteAuthStore, SiteStore, SubmissionStore};
use cumments_core::site_service::SiteService;
use cumments_reconciler::{PassConfig, PostsPass, ReconcilePass, ReconcilerDeps};
use cumments_store::DbStore;
use cumments_test_utils::TestDriver;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-retired-terminality-{}-{}.db",
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

/// A site with one room registered for the page, which the caller then retires.
async fn prepare(name: &str) -> (Arc<DbStore>, Arc<TestDriver>, SiteId, PageSlug) {
    let store = Arc::new(
        DbStore::connect(&test_db_url(name))
            .await
            .expect("connect db"),
    );
    let site_id = SiteId::from("my-blog");
    let page_slug = PageSlug::from("hello");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("provision site");
    store
        .register_room("!old:hs", &site_id, &page_slug)
        .await
        .expect("register room");
    (store, Arc::new(TestDriver::new()), site_id, page_slug)
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

fn post_command(site_id: &SiteId, page_slug: &PageSlug) -> PostCommentCommand {
    PostCommentCommand {
        site_id: site_id.clone(),
        page_slug: page_slug.clone(),
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

async fn submission_status(store: &DbStore, id: i64) -> (String, Option<String>) {
    use sea_orm::ConnectionTrait;

    let statement = sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Sqlite,
        format!("SELECT status, last_error FROM post_submissions WHERE id = {id}"),
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

/// The retirement window: a retired row is still present, so the write must
/// fail and must not resurrect the instance or materialize a replacement.
#[tokio::test]
async fn write_during_the_retirement_window_does_not_resurrect_the_retired_room() {
    let (store, driver, site_id, page_slug) = prepare("window").await;
    assert!(store.mark_room_retired("!old:hs").await.expect("retire"));

    let id = store
        .save_post_submission(&post_command(&site_id, &page_slug))
        .await
        .expect("save submission");

    let pass = PostsPass::new(reconciler_deps(&store, &driver), pass_config("posts"));
    assert_eq!(pass.run().await.expect("reconcile"), 1);

    let (status, last_error) = submission_status(&store, id).await;
    assert_eq!(status, "pending", "the write is retried, not completed");
    assert!(
        last_error.unwrap().contains("retired"),
        "the retry diagnostic must name the retired instance"
    );

    assert_eq!(
        store.get_room_status("!old:hs").await.expect("status"),
        Some(RoomStatus::Retired),
        "the retired row must not be reactivated by a write"
    );
    assert_eq!(
        store
            .get_registered_room(&site_id, &page_slug)
            .await
            .expect("active lookup"),
        None,
        "the page must have no active room during the window"
    );
    assert_eq!(
        store
            .get_room_status("!room-my-blog-hello:hs")
            .await
            .expect("status"),
        None,
        "no replacement instance may be materialized before cleanup"
    );
    assert!(
        driver.posted_messages.lock().await.is_empty(),
        "nothing may be sent to a retired room"
    );
}

/// After cleanup the same page materializes a new instance with a new room ID.
#[tokio::test]
async fn write_after_cleanup_materializes_a_new_instance() {
    let (store, driver, site_id, page_slug) = prepare("recreate").await;
    assert!(store.mark_room_retired("!old:hs").await.expect("retire"));
    // What the retirement pass does last: clear the retired instance locally.
    store
        .delete_room_local("!old:hs")
        .await
        .expect("clean up retired room");

    let id = store
        .save_post_submission(&post_command(&site_id, &page_slug))
        .await
        .expect("save submission");

    let pass = PostsPass::new(reconciler_deps(&store, &driver), pass_config("posts"));
    assert_eq!(pass.run().await.expect("reconcile"), 1);

    let (_, last_error) = submission_status(&store, id).await;
    assert!(
        !last_error.unwrap_or_default().contains("retired"),
        "the retired instance must no longer block the page"
    );

    assert_eq!(
        store
            .get_registered_room(&site_id, &page_slug)
            .await
            .expect("active lookup"),
        Some("!room-my-blog-hello:hs".to_string()),
        "recreation must register a new room instance"
    );
    assert_eq!(
        store
            .get_room_status("!room-my-blog-hello:hs")
            .await
            .expect("status"),
        Some(RoomStatus::Active)
    );
    assert_eq!(
        store.get_room_status("!old:hs").await.expect("status"),
        None,
        "the retired instance is gone, never reactivated"
    );
    assert!(
        !driver.posted_messages.lock().await.is_empty(),
        "the write proceeds against the new instance"
    );
}
