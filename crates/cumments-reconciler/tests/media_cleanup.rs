//! Integration tests for the periodic media ownership-release pass.
//!
//! The pass makes a single decision per old Cumments-owned upload: if
//! reachability evaluation proves it `Unreachable`, its `media_uploads`
//! ownership row is released. `Reachable` and `Unknown` uploads keep their
//! ownership. The pass never touches Matrix media, room or profile state, and
//! never deletes `media_references` or `media_upload_idempotency` records.

use std::sync::Arc;
use std::time::Duration;

use cumments_core::media_reference::{MediaReference, MediaReferenceSource};
use cumments_core::media_upload::MediaUploadIdempotencyInput;
use cumments_core::models::{
    AuthorKind, AuthorSnapshot, Content, MediaContent, MediaKind, Message, MessageStatus, SiteId,
    VisitorProfile,
};
use cumments_core::ports::{MediaReferenceStore, MessageStore, SiteStore};
use cumments_reconciler::{MediaCleanupPass, PassConfig, ReconcilePass, ReconcilerDeps};
use cumments_store::entities::active_enums::SubmissionStatus;
use cumments_store::entities::{media_uploads, post_submissions};
use cumments_store::sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set,
};
use cumments_store::{DbStore, sea_orm};
use cumments_test_utils::TestDriver;
use tokio::sync::Notify;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-media-cleanup-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

async fn setup(test_name: &str) -> (Arc<DbStore>, Arc<TestDriver>) {
    let store = Arc::new(
        DbStore::connect(&test_db_url(test_name))
            .await
            .expect("connect db"),
    );
    let driver = Arc::new(TestDriver::new());
    (store, driver)
}

fn build_deps(store: &Arc<DbStore>, driver: &Arc<TestDriver>) -> Arc<ReconcilerDeps> {
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
        state_redaction_repairer: driver.clone(),
        driver: driver.clone(),
        site_service: Arc::new(cumments_core::site_service::SiteService::new(
            store.clone() as Arc<dyn SiteStore>
        )),
        profile_store: None,
        media_resolver: None,
        media_reference_store: store.clone(),
    })
}

fn cleanup_pass(store: &Arc<DbStore>, driver: &Arc<TestDriver>) -> MediaCleanupPass {
    MediaCleanupPass::new(
        build_deps(store, driver),
        PassConfig {
            name: "media-cleanup-test",
            interval: Duration::from_secs(60),
            wakeup: Arc::new(Notify::new()),
        },
    )
}

async fn record_owned_upload(store: &Arc<DbStore>, site: &str, mxc: &str, author: &str) {
    store
        .record_media_upload(mxc, author, site, None)
        .await
        .expect("record upload");
}

async fn map_reference(store: &Arc<DbStore>, site: &str, mxc: &str) -> MediaReference {
    store
        .get_or_create_reference(&SiteId::from(site), mxc, MediaReferenceSource::Cumments)
        .await
        .expect("map reference")
}

/// Backdates an upload record's `created_at`, the age anchor for the pass.
async fn backdate_upload(store: &Arc<DbStore>, mxc: &str, age: chrono::Duration) {
    let created = chrono::Utc::now() - age;
    store
        .connection()
        .execute_unprepared(&format!(
            "UPDATE media_uploads SET created_at = '{}' WHERE mxc_url = '{}'",
            created.to_rfc3339(),
            mxc
        ))
        .await
        .expect("backdate upload");
}

/// Records and maps an upload old enough to be a cleanup candidate, with no
/// reachability source pointing at it (so it evaluates to `Unreachable`).
async fn old_unreachable_upload(store: &Arc<DbStore>, site: &str, mxc: &str, author: &str) {
    record_owned_upload(store, site, mxc, author).await;
    map_reference(store, site, mxc).await;
    backdate_upload(store, mxc, chrono::Duration::hours(25)).await;
}

async fn set_avatar_profile(driver: &Arc<TestDriver>, site: &str, author: &str, mxc: &str) {
    driver
        .insert_visitor_profile(
            site,
            author,
            VisitorProfile {
                display_name: Some("Visitor".to_string()),
                avatar_url: Some(mxc.to_string()),
            },
        )
        .await;
}

/// Binds the upload to a non-terminal submission whose payload references it,
/// which is the active-submission protection the evaluator must honour.
async fn bind_active_submission(store: &Arc<DbStore>, site: &str, mxc: &str) {
    let submission = post_submissions::ActiveModel {
        payload: Set(format!(r#"{{"site_id":"{site}","media":"{mxc}"}}"#)),
        status: Set(SubmissionStatus::Pending),
        retry_count: Set(0),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        timeout_confirmations: Set(0),
        timeout_check_errors: Set(0),
        ..Default::default()
    };
    let inserted = submission
        .insert(store.connection())
        .await
        .expect("insert submission");
    media_uploads::Entity::update_many()
        .col_expr(
            media_uploads::Column::SubmissionId,
            sea_orm::sea_query::Expr::value(Some(inserted.id)),
        )
        .filter(media_uploads::Column::SiteId.eq(site))
        .filter(media_uploads::Column::MxcUrl.eq(mxc))
        .exec(store.connection())
        .await
        .expect("bind submission");
}

fn media_message(event_id: &str, site: &str, mxc: &str) -> Message {
    Message {
        event_id: event_id.to_string(),
        site_id: site.to_string(),
        page_slug: "page-1".to_string(),
        author: AuthorSnapshot {
            kind: AuthorKind::Visitor,
            display_name: Some("Someone".to_string()),
            avatar_url: None,
            media_reference: None,
            public_key: Some("other-author".to_string()),
            mxid: None,
        },
        content: Content::Media(MediaContent {
            kind: MediaKind::Image,
            url: mxc.to_string(),
            filename: None,
            mimetype: None,
            size: None,
            width: None,
            height: None,
            thumbnail_url: None,
            alt_text: None,
            voice: false,
        }),
        matrix_event_type: "m.room.message".to_string(),
        timestamp: chrono::Utc::now(),
        edited_at: None,
        reply_to: None,
        thread_root: None,
        submission_id: None,
        status: MessageStatus::Active,
        redacted_at: None,
        redacted_by: None,
        reactions: Vec::new(),
        thread_summary: None,
        room_id: "!room:hs".to_string(),
        sender_mxid: "@user:hs".to_string(),
        raw_content: serde_json::json!({ "msgtype": "m.image", "url": mxc }),
    }
}

#[tokio::test]
async fn old_unreachable_upload_releases_ownership() {
    let (store, driver) = setup("old-unreachable").await;
    old_unreachable_upload(&store, "site-a", "mxc://hs/orphan", "author-1").await;

    let released = cleanup_pass(&store, &driver)
        .run()
        .await
        .expect("run cleanup pass");

    assert_eq!(
        released, 1,
        "the unreachable ownership row must be released"
    );
    assert!(
        store
            .get_media_upload("site-a", "mxc://hs/orphan")
            .await
            .unwrap()
            .is_none(),
        "released ownership record must be gone"
    );
}

#[tokio::test]
async fn old_reachable_upload_retains_ownership() {
    let (store, driver) = setup("old-reachable").await;
    let mxc = "mxc://hs/current-avatar";
    old_unreachable_upload(&store, "site-a", mxc, "author-1").await;
    set_avatar_profile(&driver, "site-a", "author-1", mxc).await;

    let released = cleanup_pass(&store, &driver)
        .run()
        .await
        .expect("run cleanup pass");

    assert_eq!(released, 0, "a reachable upload must not be released");
    assert!(
        store
            .get_media_upload("site-a", mxc)
            .await
            .unwrap()
            .is_some(),
        "reachable upload must keep its ownership record"
    );
}

#[tokio::test]
async fn old_unknown_upload_retains_ownership() {
    let (store, driver) = setup("old-unknown").await;
    let mxc = "mxc://hs/unknown";
    old_unreachable_upload(&store, "site-a", mxc, "author-1").await;
    *driver.fail_get_profile.lock().await = true;

    let released = cleanup_pass(&store, &driver)
        .run()
        .await
        .expect("run cleanup pass");

    assert_eq!(released, 0, "unknown reachability must never be released");
    assert!(
        store
            .get_media_upload("site-a", mxc)
            .await
            .unwrap()
            .is_some(),
        "unknown upload must keep its ownership record"
    );
}

#[tokio::test]
async fn too_young_unreachable_upload_retains_ownership() {
    let (store, driver) = setup("too-young").await;
    let mxc = "mxc://hs/fresh";
    // Unreachable but recorded just now, so it is inside the grace period.
    record_owned_upload(&store, "site-a", mxc, "author-1").await;
    map_reference(&store, "site-a", mxc).await;

    let released = cleanup_pass(&store, &driver)
        .run()
        .await
        .expect("run cleanup pass");

    assert_eq!(released, 0, "uploads inside the grace period must be kept");
    assert!(
        store
            .get_media_upload("site-a", mxc)
            .await
            .unwrap()
            .is_some(),
        "young upload must keep its ownership record"
    );
}

#[tokio::test]
async fn active_submission_protects_otherwise_unreachable_upload() {
    let (store, driver) = setup("active-submission").await;
    let mxc = "mxc://hs/pending-upload";
    record_owned_upload(&store, "site-a", mxc, "author-1").await;
    map_reference(&store, "site-a", mxc).await;
    bind_active_submission(&store, "site-a", mxc).await;
    backdate_upload(&store, mxc, chrono::Duration::hours(25)).await;

    let released = cleanup_pass(&store, &driver)
        .run()
        .await
        .expect("run cleanup pass");

    assert_eq!(released, 0, "active submission must protect the upload");
    assert!(
        store
            .get_media_upload("site-a", mxc)
            .await
            .unwrap()
            .is_some(),
        "submission-bound upload must keep its ownership record"
    );
}

#[tokio::test]
async fn unevaluable_candidate_does_not_block_other_releases() {
    let (store, driver) = setup("continue-past-unknown").await;

    // The first candidate is old, mapped and unreachable: it must be released.
    old_unreachable_upload(&store, "site-a", "mxc://hs/releasable", "author-1").await;

    // The second candidate is old but has no media_references mapping, so the
    // evaluator can only return `Unknown` for it. That per-candidate failure
    // must not abort the pass before the releasable candidate is processed.
    record_owned_upload(&store, "site-a", "mxc://hs/unmapped", "author-2").await;
    backdate_upload(&store, "mxc://hs/unmapped", chrono::Duration::hours(25)).await;

    let released = cleanup_pass(&store, &driver)
        .run()
        .await
        .expect("run cleanup pass");

    assert_eq!(
        released, 1,
        "the evaluable candidate must still be released"
    );
    assert!(
        store
            .get_media_upload("site-a", "mxc://hs/releasable")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_media_upload("site-a", "mxc://hs/unmapped")
            .await
            .unwrap()
            .is_some(),
        "the unevaluable candidate must keep its ownership record"
    );
}

#[tokio::test]
async fn release_targets_only_the_matching_ownership_row() {
    let (store, driver) = setup("matching-row").await;
    old_unreachable_upload(&store, "site-a", "mxc://hs/target", "author-1").await;

    // Another upload on the same site stays reachable through the profile.
    let keeper = "mxc://hs/keeper";
    record_owned_upload(&store, "site-a", keeper, "author-2").await;
    map_reference(&store, "site-a", keeper).await;
    backdate_upload(&store, keeper, chrono::Duration::hours(25)).await;
    set_avatar_profile(&driver, "site-a", "author-2", keeper).await;

    let released = cleanup_pass(&store, &driver)
        .run()
        .await
        .expect("run cleanup pass");

    assert_eq!(released, 1);
    assert!(
        store
            .get_media_upload("site-a", "mxc://hs/target")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_media_upload("site-a", keeper)
            .await
            .unwrap()
            .is_some(),
        "an unrelated ownership row must not be affected"
    );
}

#[tokio::test]
async fn cross_site_reference_neither_blocks_nor_overreaches() {
    let (store, driver) = setup("cross-site").await;
    let shared = "mxc://hs/shared-mxc";

    // site-a owns the shared MXC and it is unreachable from site-a's perspective.
    old_unreachable_upload(&store, "site-a", shared, "author-a").await;

    // site-b also knows the same MXC: it has a mapping and a content reference.
    map_reference(&store, "site-b", shared).await;
    store
        .save_message(&media_message("$msg_b", "site-b", shared))
        .await
        .unwrap();

    // site-b separately owns a reachable upload that must survive.
    let site_b_owned = "mxc://hs/site-b-avatar";
    record_owned_upload(&store, "site-b", site_b_owned, "author-b").await;
    map_reference(&store, "site-b", site_b_owned).await;
    backdate_upload(&store, site_b_owned, chrono::Duration::hours(25)).await;
    set_avatar_profile(&driver, "site-b", "author-b", site_b_owned).await;

    let released = cleanup_pass(&store, &driver)
        .run()
        .await
        .expect("run cleanup pass");

    assert_eq!(released, 1, "only site-a's ownership row is eligible");
    assert!(
        store
            .get_media_upload("site-a", shared)
            .await
            .unwrap()
            .is_none(),
        "site-a's ownership of the shared MXC must be released"
    );
    assert!(
        store
            .find_reference(&SiteId::from("site-b"), shared)
            .await
            .unwrap()
            .is_some(),
        "site-b's mapping of the shared MXC must be preserved"
    );
    assert!(
        store
            .has_content_attachment("site-b", shared)
            .await
            .unwrap(),
        "site-b's content reference to the shared MXC must be preserved"
    );
    assert!(
        store
            .get_media_upload("site-b", site_b_owned)
            .await
            .unwrap()
            .is_some(),
        "site-b's reachable ownership row must be preserved"
    );
}

#[tokio::test]
async fn two_sites_owning_the_same_mxc_are_released_independently() {
    let (store, driver) = setup("same-mxc-two-sites").await;
    let shared = "mxc://hs/owned-by-both";

    // site-a owns the MXC but does not reference it: releasable.
    old_unreachable_upload(&store, "site-a", shared, "author-a").await;

    // site-b owns the same MXC and keeps it reachable through the profile.
    record_owned_upload(&store, "site-b", shared, "author-b").await;
    map_reference(&store, "site-b", shared).await;
    backdate_upload(&store, shared, chrono::Duration::hours(25)).await;
    set_avatar_profile(&driver, "site-b", "author-b", shared).await;

    let released = cleanup_pass(&store, &driver)
        .run()
        .await
        .expect("run cleanup pass");

    assert_eq!(released, 1, "only site-a's ownership is eligible");
    assert!(
        store
            .get_media_upload("site-a", shared)
            .await
            .unwrap()
            .is_none(),
        "site-a's unreachable ownership must be released"
    );
    assert!(
        store
            .get_media_upload("site-b", shared)
            .await
            .unwrap()
            .is_some(),
        "site-b's reachable ownership of the same MXC must survive"
    );
}

#[tokio::test]
async fn used_at_is_not_a_release_signal() {
    let (store, driver) = setup("used-at").await;

    // A "used" upload that is unreachable is still released: `used_at` never
    // gates the decision.
    let used = "mxc://hs/marked-used";
    old_unreachable_upload(&store, "site-a", used, "author-1").await;
    store.mark_media_used("site-a", used).await.unwrap();

    // An upload with `used_at` still NULL but a live profile reference is kept.
    let live = "mxc://hs/never-used";
    old_unreachable_upload(&store, "site-a", live, "author-2").await;
    set_avatar_profile(&driver, "site-a", "author-2", live).await;

    let released = cleanup_pass(&store, &driver)
        .run()
        .await
        .expect("run cleanup pass");

    assert_eq!(released, 1);
    assert!(
        store
            .get_media_upload("site-a", used)
            .await
            .unwrap()
            .is_none(),
        "used_at must not block release of an unreachable upload"
    );
    assert!(
        store
            .get_media_upload("site-a", live)
            .await
            .unwrap()
            .is_some(),
        "a NULL used_at must not by itself release an upload"
    );
}

#[tokio::test]
async fn release_preserves_references_idempotency_and_matrix_state() {
    let (store, driver) = setup("non-destructive").await;
    let site = "site-a";
    let mxc = "mxc://hs/asset";
    let author = "author-1";
    let idem_key = "upload-key-1";

    store
        .save_media_upload_idempotent(
            mxc,
            author,
            site,
            Some("page-1"),
            &MediaUploadIdempotencyInput {
                key: idem_key.to_string(),
                request_fingerprint: "fingerprint-1".to_string(),
            },
        )
        .await
        .expect("record idempotent upload");
    let media_ref = map_reference(&store, site, mxc).await;
    backdate_upload(&store, mxc, chrono::Duration::hours(25)).await;

    let site_id = SiteId::from(site);
    let pre_reference = store.get_record(&site_id, &media_ref).await.unwrap();

    let released = cleanup_pass(&store, &driver)
        .run()
        .await
        .expect("run cleanup pass");

    assert_eq!(released, 1);

    // media_references are untouched.
    assert_eq!(
        store.get_record(&site_id, &media_ref).await.unwrap(),
        pre_reference,
        "media_references must never be modified by ownership release"
    );

    // media_upload_idempotency records are untouched.
    let idempotency = store
        .find_media_upload_idempotency(author, idem_key)
        .await
        .unwrap()
        .expect("idempotency record must survive ownership release");
    assert_eq!(idempotency.mxc_url, mxc);
    assert_eq!(idempotency.request_fingerprint, "fingerprint-1");

    // No Matrix-side mutation (including media deletion) was attempted.
    assert!(driver.avatar_updates.lock().await.is_empty());
    assert!(driver.state_writes.lock().await.is_empty());
    assert!(driver.redactions.lock().await.is_empty());
    assert!(driver.posted_messages.lock().await.is_empty());
}
