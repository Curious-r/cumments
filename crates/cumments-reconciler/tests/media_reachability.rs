//! Stage H1 integration tests for MediaReachabilityEvaluator.
//!
//! Validates:
//! 1. Logical reachability evaluation across Current Global Profile, Historical Author Presentation,
//!    and Content Attachments (messages, revisions, sticker packs, active submissions).
//! 2. Independent ownership determination (CummentsOwned vs NotOwned).
//! 3. Conservative safety: lookup failures and unmapped uploads produce `Unknown` (never `Unreachable`).
//! 4. Cross-site isolation: references in site-B never make candidates in site-A reachable.
//! 5. External media protection: external MediaReferences never enter owned candidates,
//!    and even when unreachable, are never proposed as cleanup eligible.
//! 6. Non-destructive guarantee: evaluations do not mutate local or Matrix state.

use std::sync::Arc;

use cumments_core::media_reachability::{
    MediaOwnership, MediaReachabilityEvaluator, ReachabilityState,
};
use cumments_core::models::{
    AuthorKind, AuthorSnapshot, Content, MediaContent, MediaKind, Message, MessageStatus, SiteId,
    TextContent, TextStyle, VisitorProfile,
};
use cumments_core::ports::{MediaReferenceStore, MessageStore, SiteStore};
use cumments_store::entities::active_enums::SubmissionStatus;
use cumments_store::entities::{media_uploads, message_revisions, post_submissions, sticker_packs};
use cumments_store::sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use cumments_store::{DbStore, sea_orm};
use cumments_test_utils::TestDriver;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-media-reachability-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

async fn setup_env(test_name: &str) -> (Arc<DbStore>, Arc<TestDriver>, MediaReachabilityEvaluator) {
    let db_url = test_db_url(test_name);
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let driver = Arc::new(TestDriver::new());
    let evaluator = MediaReachabilityEvaluator::new(
        driver.clone(),
        store.clone() as Arc<dyn MediaReferenceStore>,
        store.clone() as Arc<dyn MessageStore>,
    );
    (store, driver, evaluator)
}

fn test_message(
    event_id: &str,
    site_id: &str,
    page_slug: &str,
    author: AuthorSnapshot,
    content: Content,
) -> Message {
    Message {
        event_id: event_id.to_string(),
        site_id: site_id.to_string(),
        page_slug: page_slug.to_string(),
        author,
        content,
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
        raw_content: serde_json::json!({ "msgtype": "m.text", "body": "test" }),
    }
}

#[tokio::test]
async fn current_profile_referenced_yields_reachable() {
    let (store, driver, evaluator) = setup_env("profile_ref").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let author_pubkey = "author-pubkey-1";
    let candidate_mxc = "mxc://hs/avatar-123";

    // 1. Record upload and mapping
    store
        .record_media_upload(candidate_mxc, author_pubkey, site_id.as_str(), None)
        .await
        .unwrap();
    let media_ref = store
        .get_or_create_reference(&site_id, candidate_mxc)
        .await
        .unwrap();

    // 2. Authoritative Matrix profile has candidate avatar
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author_pubkey.to_string()),
        VisitorProfile {
            display_name: Some("Alice".to_string()),
            avatar_url: Some(candidate_mxc.to_string()),
        },
    );

    let result = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    assert_eq!(result.ownership, MediaOwnership::CummentsOwned);
    assert_eq!(result.media_reference, Some(media_ref));
    assert_eq!(
        result.reachability.current_profile,
        ReachabilityState::Reachable
    );
    assert_eq!(
        result.reachability.historical_presentation,
        ReachabilityState::Unreachable
    );
    assert_eq!(
        result.reachability.content_attachment,
        ReachabilityState::Unreachable
    );
    assert_eq!(result.reachability.overall, ReachabilityState::Reachable);
    assert!(!result.is_cleanup_eligible());
}

#[tokio::test]
async fn current_profile_replaced_or_unset_yields_unreachable() {
    let (store, driver, evaluator) = setup_env("profile_replaced_or_unset").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let author_pubkey = "author-pubkey-2";
    let candidate_mxc = "mxc://hs/avatar-old";

    store
        .record_media_upload(candidate_mxc, author_pubkey, site_id.as_str(), None)
        .await
        .unwrap();
    store
        .get_or_create_reference(&site_id, candidate_mxc)
        .await
        .unwrap();

    // Case A: Profile replaced with different avatar
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author_pubkey.to_string()),
        VisitorProfile {
            display_name: Some("Bob".to_string()),
            avatar_url: Some("mxc://hs/avatar-new".to_string()),
        },
    );

    let result = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    assert_eq!(
        result.reachability.current_profile,
        ReachabilityState::Unreachable
    );
    assert_eq!(result.reachability.overall, ReachabilityState::Unreachable);
    assert!(result.is_cleanup_eligible());

    // Case B: Profile avatar unset (None)
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author_pubkey.to_string()),
        VisitorProfile {
            display_name: Some("Bob".to_string()),
            avatar_url: None,
        },
    );

    let result_unset = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    assert_eq!(
        result_unset.reachability.current_profile,
        ReachabilityState::Unreachable
    );
    assert_eq!(
        result_unset.reachability.overall,
        ReachabilityState::Unreachable
    );
    assert!(result_unset.is_cleanup_eligible());
}

#[tokio::test]
async fn current_profile_lookup_failure_yields_unknown() {
    let (store, driver, evaluator) = setup_env("profile_failure").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let author_pubkey = "author-pubkey-3";
    let candidate_mxc = "mxc://hs/avatar-fail";

    store
        .record_media_upload(candidate_mxc, author_pubkey, site_id.as_str(), None)
        .await
        .unwrap();
    store
        .get_or_create_reference(&site_id, candidate_mxc)
        .await
        .unwrap();

    // Simulate homeserver failure
    *driver.fail_get_profile.lock().await = true;

    let result = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    assert_eq!(
        result.reachability.current_profile,
        ReachabilityState::Unknown
    );
    assert_eq!(result.reachability.overall, ReachabilityState::Unknown);
    assert!(
        !result.is_cleanup_eligible(),
        "Unknown reachability must never be cleanup-eligible"
    );
}

#[tokio::test]
async fn historical_presentation_referenced_yields_reachable() {
    let (store, driver, evaluator) = setup_env("hist_ref").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let author_pubkey = "author-pubkey-4";
    let candidate_mxc = "mxc://hs/avatar-historical";

    store
        .record_media_upload(candidate_mxc, author_pubkey, site_id.as_str(), None)
        .await
        .unwrap();
    let media_ref = store
        .get_or_create_reference(&site_id, candidate_mxc)
        .await
        .unwrap();

    // Current profile has changed to something else
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author_pubkey.to_string()),
        VisitorProfile {
            display_name: Some("Charlie".to_string()),
            avatar_url: Some("mxc://hs/avatar-newer".to_string()),
        },
    );

    // Save historical message with author_media_reference
    let message = test_message(
        "$hist_event_1",
        site_id.as_str(),
        "page-1",
        AuthorSnapshot {
            kind: AuthorKind::Visitor,
            display_name: Some("Charlie".to_string()),
            avatar_url: Some(candidate_mxc.to_string()),
            media_reference: Some(media_ref.clone()),
            public_key: Some(author_pubkey.to_string()),
            mxid: None,
        },
        Content::Text(TextContent {
            body: "A historical comment".to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        }),
    );
    store.save_message(&message).await.unwrap();

    let result = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    assert_eq!(
        result.reachability.current_profile,
        ReachabilityState::Unreachable
    );
    assert_eq!(
        result.reachability.historical_presentation,
        ReachabilityState::Reachable
    );
    assert_eq!(
        result.reachability.content_attachment,
        ReachabilityState::Unreachable
    );
    assert_eq!(result.reachability.overall, ReachabilityState::Reachable);
    assert!(!result.is_cleanup_eligible());
}

#[tokio::test]
async fn content_attachment_in_message_body_yields_reachable() {
    let (store, _driver, evaluator) = setup_env("content_msg").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let author_pubkey = "author-pubkey-5";
    let candidate_mxc = "mxc://hs/image-embedded";

    store
        .record_media_upload(
            candidate_mxc,
            author_pubkey,
            site_id.as_str(),
            Some("page-1"),
        )
        .await
        .unwrap();
    store
        .get_or_create_reference(&site_id, candidate_mxc)
        .await
        .unwrap();

    // Message references candidate_mxc in its content
    let message = test_message(
        "$msg_content_1",
        site_id.as_str(),
        "page-1",
        AuthorSnapshot {
            kind: AuthorKind::Visitor,
            display_name: Some("David".to_string()),
            avatar_url: None,
            media_reference: None,
            public_key: Some("other-pubkey".to_string()),
            mxid: None,
        },
        Content::Media(MediaContent {
            kind: MediaKind::Image,
            url: candidate_mxc.to_string(),
            filename: Some("photo.png".to_string()),
            mimetype: Some("image/png".to_string()),
            size: Some(1024),
            width: None,
            height: None,
            thumbnail_url: None,
            alt_text: None,
            voice: false,
        }),
    );
    store.save_message(&message).await.unwrap();

    let result = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    assert_eq!(
        result.reachability.content_attachment,
        ReachabilityState::Reachable
    );
    assert_eq!(result.reachability.overall, ReachabilityState::Reachable);
    assert!(!result.is_cleanup_eligible());
}

#[tokio::test]
async fn content_attachment_in_revision_yields_reachable() {
    let (store, _driver, evaluator) = setup_env("content_revision").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let candidate_mxc = "mxc://hs/image-in-revision";
    store
        .record_media_upload(candidate_mxc, "author-pubkey", site_id.as_str(), None)
        .await
        .unwrap();
    store
        .get_or_create_reference(&site_id, candidate_mxc)
        .await
        .unwrap();

    // Base message is text
    let message = test_message(
        "$msg_rev_parent",
        site_id.as_str(),
        "page-1",
        AuthorSnapshot {
            kind: AuthorKind::Visitor,
            display_name: None,
            avatar_url: None,
            media_reference: None,
            public_key: None,
            mxid: None,
        },
        Content::Text(TextContent {
            body: "Original text".to_string(),
            formatted_body: None,
            style: TextStyle::Normal,
        }),
    );
    store.save_message(&message).await.unwrap();

    // Revision contains candidate_mxc
    let rev_model = message_revisions::ActiveModel {
        event_id: Set("$edit_event_1".to_string()),
        message_event_id: Set("$msg_rev_parent".to_string()),
        content_json: Set(format!(r#"{{"type":"media","url":"{candidate_mxc}"}}"#)),
        edited_at: Set(chrono::Utc::now()),
        editor_mxid: Set("@user:hs".to_string()),
        redacted_at: Set(None),
        redacted_by: Set(None),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    };
    rev_model.insert(store.connection()).await.unwrap();

    let result = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    assert_eq!(
        result.reachability.content_attachment,
        ReachabilityState::Reachable
    );
    assert_eq!(result.reachability.overall, ReachabilityState::Reachable);
}

#[tokio::test]
async fn content_attachment_in_sticker_pack_yields_reachable() {
    let (store, _driver, evaluator) = setup_env("content_sticker").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let candidate_mxc = "mxc://hs/sticker-123";
    store
        .record_media_upload(candidate_mxc, "author-pubkey", site_id.as_str(), None)
        .await
        .unwrap();
    store
        .get_or_create_reference(&site_id, candidate_mxc)
        .await
        .unwrap();

    // Insert sticker pack referencing candidate_mxc
    let pack = sticker_packs::ActiveModel {
        site_id: Set(site_id.as_str().to_string()),
        state_key: Set("custom_stickers".to_string()),
        room_id: Set("!space:hs".to_string()),
        event_id: Set("$pack_event_1".to_string()),
        sender: Set("@admin:hs".to_string()),
        origin_server_ts: Set(1000),
        pack_json: Set(format!(
            r#"{{"images":{{"happy":{{"url":"{candidate_mxc}"}}}}}}"#
        )),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
    };
    pack.insert(store.connection()).await.unwrap();

    let result = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    assert_eq!(
        result.reachability.content_attachment,
        ReachabilityState::Reachable
    );
    assert_eq!(result.reachability.overall, ReachabilityState::Reachable);
}

#[tokio::test]
async fn content_attachment_in_active_post_submission_yields_reachable() {
    let (store, _driver, evaluator) = setup_env("content_active_sub").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let candidate_mxc = "mxc://hs/upload-in-submission";
    store
        .record_media_upload(candidate_mxc, "author-pubkey", site_id.as_str(), None)
        .await
        .unwrap();
    store
        .get_or_create_reference(&site_id, candidate_mxc)
        .await
        .unwrap();

    // Create an active post submission
    let sub = post_submissions::ActiveModel {
        payload: Set(format!(
            r#"{{"site_id":"{}","media":"{}"}}"#,
            site_id.as_str(),
            candidate_mxc
        )),
        status: Set(SubmissionStatus::Pending),
        retry_count: Set(0),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        timeout_confirmations: Set(0),
        timeout_check_errors: Set(0),
        ..Default::default()
    };
    let inserted_sub = sub.insert(store.connection()).await.unwrap();

    // Link submission to media_upload
    media_uploads::Entity::update_many()
        .col_expr(
            media_uploads::Column::SubmissionId,
            sea_orm::sea_query::Expr::value(Some(inserted_sub.id)),
        )
        .filter(media_uploads::Column::SiteId.eq(site_id.as_str()))
        .filter(media_uploads::Column::MxcUrl.eq(candidate_mxc))
        .exec(store.connection())
        .await
        .unwrap();

    let result = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    assert_eq!(
        result.reachability.content_attachment,
        ReachabilityState::Reachable
    );
    assert_eq!(result.reachability.overall, ReachabilityState::Reachable);
}

#[tokio::test]
async fn combined_all_absent_yields_unreachable_and_cleanup_eligible() {
    let (store, _driver, evaluator) = setup_env("combined_unreachable").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let candidate_mxc = "mxc://hs/orphan-candidate";
    store
        .record_media_upload(candidate_mxc, "author-pubkey", site_id.as_str(), None)
        .await
        .unwrap();
    store
        .get_or_create_reference(&site_id, candidate_mxc)
        .await
        .unwrap();

    let result = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    assert_eq!(result.ownership, MediaOwnership::CummentsOwned);
    assert_eq!(
        result.reachability.current_profile,
        ReachabilityState::Unreachable
    );
    assert_eq!(
        result.reachability.historical_presentation,
        ReachabilityState::Unreachable
    );
    assert_eq!(
        result.reachability.content_attachment,
        ReachabilityState::Unreachable
    );
    assert_eq!(result.reachability.overall, ReachabilityState::Unreachable);
    assert!(
        result.is_cleanup_eligible(),
        "Unreachable Cumments-owned upload must be cleanup eligible"
    );
}

#[tokio::test]
async fn combined_one_unknown_others_absent_yields_unknown_and_not_eligible() {
    let (store, driver, evaluator) = setup_env("combined_unknown").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let candidate_mxc = "mxc://hs/candidate-with-unknown";
    store
        .record_media_upload(candidate_mxc, "author-pubkey", site_id.as_str(), None)
        .await
        .unwrap();
    store
        .get_or_create_reference(&site_id, candidate_mxc)
        .await
        .unwrap();

    // Profile lookup fails -> Unknown
    *driver.fail_get_profile.lock().await = true;

    let result = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    assert_eq!(
        result.reachability.current_profile,
        ReachabilityState::Unknown
    );
    assert_eq!(
        result.reachability.historical_presentation,
        ReachabilityState::Unreachable
    );
    assert_eq!(
        result.reachability.content_attachment,
        ReachabilityState::Unreachable
    );
    assert_eq!(result.reachability.overall, ReachabilityState::Unknown);
    assert!(
        !result.is_cleanup_eligible(),
        "Candidate with Unknown reachability must NOT be cleanup eligible"
    );
}

#[tokio::test]
async fn unmapped_upload_yields_cumments_owned_and_unknown_reachability() {
    let (store, _driver, evaluator) = setup_env("unmapped_upload").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let candidate_mxc = "mxc://hs/unmapped-upload";
    // Upload recorded, but NO media_references mapping created
    store
        .record_media_upload(candidate_mxc, "author-pubkey", site_id.as_str(), None)
        .await
        .unwrap();

    let result = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    assert_eq!(result.ownership, MediaOwnership::CummentsOwned);
    assert_eq!(result.media_reference, None);
    assert_eq!(result.reachability.overall, ReachabilityState::Unknown);
    assert!(
        !result.is_cleanup_eligible(),
        "Unmapped upload must produce Unknown and never be cleanup eligible"
    );
}

#[tokio::test]
async fn external_media_is_not_owned_and_never_eligible() {
    let (store, _driver, evaluator) = setup_env("external_media").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let ext_mxc = "mxc://hs/external-discovered-avatar";
    // External mapping created, NO media_uploads record
    let media_ref = store
        .get_or_create_reference(&site_id, ext_mxc)
        .await
        .unwrap();

    let result = evaluator.evaluate_candidate(&site_id, ext_mxc).await;
    assert_eq!(result.ownership, MediaOwnership::NotOwned);
    assert_eq!(result.media_reference, Some(media_ref));
    assert_eq!(result.reachability.overall, ReachabilityState::Unreachable);
    assert!(
        !result.is_cleanup_eligible(),
        "External unreachable media must NEVER be cleanup eligible"
    );

    // Also verify evaluate_site_owned_candidates never includes external media
    let owned_candidates = evaluator
        .evaluate_site_owned_candidates(&site_id)
        .await
        .unwrap();
    assert!(
        owned_candidates.is_empty(),
        "External media must never enter the owned-upload candidate set"
    );
}

#[tokio::test]
async fn cross_site_isolation_preserves_independent_reachability() {
    let (store, _driver, evaluator) = setup_env("cross_site").await;
    let site_a = SiteId::from("site-a");
    let site_b = SiteId::from("site-b");
    store
        .ensure_site_exists(site_a.as_str(), "!space-a:hs")
        .await
        .unwrap();
    store
        .ensure_site_exists(site_b.as_str(), "!space-b:hs")
        .await
        .unwrap();

    let candidate_mxc = "mxc://hs/shared-mxc-string";

    // Uploaded for site-a
    store
        .record_media_upload(candidate_mxc, "author-a", site_a.as_str(), None)
        .await
        .unwrap();
    store
        .get_or_create_reference(&site_a, candidate_mxc)
        .await
        .unwrap();

    // site-b references candidate_mxc in a comment!
    let message_on_b = test_message(
        "$msg_on_b",
        site_b.as_str(),
        "page-b",
        AuthorSnapshot {
            kind: AuthorKind::Visitor,
            display_name: Some("User B".to_string()),
            avatar_url: None,
            media_reference: None,
            public_key: None,
            mxid: None,
        },
        Content::Media(MediaContent {
            kind: MediaKind::Image,
            url: candidate_mxc.to_string(),
            filename: None,
            mimetype: None,
            size: None,
            width: None,
            height: None,
            thumbnail_url: None,
            alt_text: None,
            voice: false,
        }),
    );
    store.save_message(&message_on_b).await.unwrap();

    // Evaluating site-a must NOT be affected by site-b's reference!
    let result_a = evaluator.evaluate_candidate(&site_a, candidate_mxc).await;
    assert_eq!(
        result_a.reachability.content_attachment,
        ReachabilityState::Unreachable,
        "site-b content reference must not affect site-a reachability"
    );
    assert_eq!(
        result_a.reachability.overall,
        ReachabilityState::Unreachable
    );
    assert!(result_a.is_cleanup_eligible());
}

#[tokio::test]
async fn read_only_guarantee_preserves_all_durable_state() {
    let (store, _driver, evaluator) = setup_env("read_only").await;
    let site_id = SiteId::from("site-a");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .unwrap();

    let candidate_mxc = "mxc://hs/read-only-check";
    store
        .record_media_upload(candidate_mxc, "author-1", site_id.as_str(), None)
        .await
        .unwrap();
    let media_ref = store
        .get_or_create_reference(&site_id, candidate_mxc)
        .await
        .unwrap();

    let pre_uploads = store
        .list_media_urls_for_site(site_id.as_str())
        .await
        .unwrap();
    let pre_ref = store.get_record(&site_id, &media_ref).await.unwrap();

    // Run evaluation multiple times
    let _ = evaluator.evaluate_candidate(&site_id, candidate_mxc).await;
    let _ = evaluator
        .evaluate_site_owned_candidates(&site_id)
        .await
        .unwrap();

    let post_uploads = store
        .list_media_urls_for_site(site_id.as_str())
        .await
        .unwrap();
    let post_ref = store.get_record(&site_id, &media_ref).await.unwrap();

    assert_eq!(
        pre_uploads, post_uploads,
        "media_uploads must not be mutated"
    );
    assert_eq!(pre_ref, post_ref, "media_references must not be mutated");
}
