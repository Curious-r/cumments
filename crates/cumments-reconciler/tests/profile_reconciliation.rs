//! Integration tests for ExternalProfileReconciler in cumments-reconciler.

use cumments_core::media_reference::MediaReferenceSource;
use cumments_core::models::{SiteId, VisitorProfile};
use cumments_core::ports::{MediaReferenceStore, MessageStore, SiteStore, VirtualUserStore};
use cumments_reconciler::ExternalProfileReconciler;
use cumments_store::DbStore;
use cumments_test_utils::TestDriver;
use std::sync::Arc;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-reconciler-profile-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn external_profile_reconciler_distinguishes_provenance_and_preserves_durability() {
    let db_url = test_db_url("distinguish_provenance");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("my-site");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");

    let driver = Arc::new(TestDriver::new());
    let reconciler = ExternalProfileReconciler::new(
        driver.clone(),
        store.clone() as Arc<dyn MediaReferenceStore>,
        store.clone() as Arc<dyn MessageStore>,
    );

    // 1. Authoritative Matrix profile observation with external avatar MXC
    let author_ext = "author-ext-pubkey-1";
    let ext_mxc = "mxc://hs/matrix-external-avatar-111";
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author_ext.to_string()),
        VisitorProfile {
            display_name: Some("External User".to_string()),
            avatar_url: Some(ext_mxc.to_string()),
        },
    );

    let media_ref = reconciler
        .reconcile_visitor_profile(&site_id, author_ext)
        .await
        .expect("reconcile external profile")
        .expect("media ref exists");

    assert!(media_ref.as_str().starts_with("cumments-media:"));
    let rec = store
        .get_record(&site_id, &media_ref)
        .await
        .expect("get record")
        .expect("record exists");
    assert!(
        rec.is_external,
        "unmapped Matrix global profile avatar must be recorded as is_external = true"
    );
    assert_eq!(rec.source(), MediaReferenceSource::External);
    assert_eq!(rec.mxc_uri, ext_mxc);

    // 2. Idempotency: repeated observation reuses exact same reference
    let media_ref_repeated = reconciler
        .reconcile_visitor_profile(&site_id, author_ext)
        .await
        .expect("repeat reconcile")
        .expect("media ref exists");
    assert_eq!(media_ref, media_ref_repeated);

    // 3. Cumments-owned upload: present in media_uploads -> recorded as is_external = false
    let author_cumments = "author-cumments-pubkey-2";
    let cumments_mxc = "mxc://hs/cumments-uploaded-avatar-222";
    store
        .record_media_upload(
            cumments_mxc,
            author_cumments,
            site_id.as_str(),
            Some("post-1"),
        )
        .await
        .expect("record media upload");

    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author_cumments.to_string()),
        VisitorProfile {
            display_name: Some("Cumments User".to_string()),
            avatar_url: Some(cumments_mxc.to_string()),
        },
    );

    let cumments_ref = reconciler
        .reconcile_visitor_profile(&site_id, author_cumments)
        .await
        .expect("reconcile cumments profile")
        .expect("media ref exists");

    let cumments_rec = store
        .get_record(&site_id, &cumments_ref)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !cumments_rec.is_external,
        "avatar backed by authoritative local upload must be is_external = false"
    );
    assert_eq!(cumments_rec.source(), MediaReferenceSource::Cumments);

    // 4. Stability: subsequent observation of existing mappings never overwrites provenance
    // Re-observing cumments_mxc remains is_external = false
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), "another-author".to_string()),
        VisitorProfile {
            display_name: Some("Another User".to_string()),
            avatar_url: Some(cumments_mxc.to_string()),
        },
    );
    let reobserved_cumments = reconciler
        .reconcile_visitor_profile(&site_id, "another-author")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cumments_ref, reobserved_cumments);
    let post_rec = store
        .get_record(&site_id, &cumments_ref)
        .await
        .unwrap()
        .unwrap();
    assert!(!post_rec.is_external);

    // 5. Explicit external entrypoint
    let direct_mxc = "mxc://hs/direct-ext-avatar-333";
    let direct_ref = reconciler
        .reconcile_external_profile_avatar(&site_id, direct_mxc)
        .await
        .expect("reconcile direct");
    let direct_rec = store
        .get_record(&site_id, &direct_ref)
        .await
        .unwrap()
        .unwrap();
    assert!(direct_rec.is_external);

    // 6. Profile with None avatar returns None
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), "no-avatar-user".to_string()),
        VisitorProfile {
            display_name: Some("No Avatar".to_string()),
            avatar_url: None,
        },
    );
    assert!(
        reconciler
            .reconcile_visitor_profile(&site_id, "no-avatar-user")
            .await
            .unwrap()
            .is_none()
    );

    // 7. Non-existent user profile returns None
    assert!(
        reconciler
            .reconcile_visitor_profile(&site_id, "unknown-author")
            .await
            .unwrap()
            .is_none()
    );

    // 8. Ownership table untouched
    let is_owned = store
        .media_upload_owned_by(ext_mxc, "@someone:hs", site_id.as_str(), "page")
        .await
        .expect("check owned");
    assert!(
        !is_owned,
        "external discovery must not create media_uploads"
    );
    let is_direct_owned = store
        .media_upload_owned_by(direct_mxc, "@someone:hs", site_id.as_str(), "page")
        .await
        .expect("check owned");
    assert!(
        !is_direct_owned,
        "direct external discovery must not create media_uploads"
    );

    // 9. Restart durability: new reconciler instance on same DB returns same reference
    let driver_v2 = Arc::new(TestDriver::new());
    driver_v2.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author_ext.to_string()),
        VisitorProfile {
            display_name: Some("External User".to_string()),
            avatar_url: Some(ext_mxc.to_string()),
        },
    );
    let reconciler_v2 = ExternalProfileReconciler::new(
        driver_v2,
        store.clone() as Arc<dyn MediaReferenceStore>,
        store.clone() as Arc<dyn MessageStore>,
    );
    let restarted_ref = reconciler_v2
        .reconcile_visitor_profile(&site_id, author_ext)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(media_ref, restarted_ref);
}

#[tokio::test]
async fn external_profile_reconciler_reconciles_virtual_user_profile() {
    let db_url = test_db_url("reconcile_virtual_user");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));

    let site_id = SiteId::from("vu-site");
    store
        .ensure_site_exists(site_id.as_str(), "!space:hs")
        .await
        .expect("ensure site");

    let author_pubkey = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc";
    let virtual_user_id = store
        .get_or_create_virtual_user(author_pubkey, &site_id, "hs")
        .await
        .expect("create virtual user");

    let driver = Arc::new(TestDriver::new());
    let ext_mxc = "mxc://hs/virtual-user-ext-avatar-999";
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author_pubkey.to_string()),
        VisitorProfile {
            display_name: Some("Virtual User External".to_string()),
            avatar_url: Some(ext_mxc.to_string()),
        },
    );

    // 1. Without VirtualUserStore wired: returns None
    let unconfigured_reconciler = ExternalProfileReconciler::new(
        driver.clone(),
        store.clone() as Arc<dyn MediaReferenceStore>,
        store.clone() as Arc<dyn MessageStore>,
    );
    assert!(
        unconfigured_reconciler
            .reconcile_virtual_user_profile(&site_id, &virtual_user_id)
            .await
            .unwrap()
            .is_none()
    );

    // 2. With VirtualUserStore wired: resolves and reconciles external avatar
    let configured_reconciler =
        unconfigured_reconciler.with_virtual_user_store(store.clone() as Arc<dyn VirtualUserStore>);

    let media_ref = configured_reconciler
        .reconcile_virtual_user_profile(&site_id, &virtual_user_id)
        .await
        .expect("reconcile virtual user profile")
        .expect("media ref resolved");

    assert!(media_ref.as_str().starts_with("cumments-media:"));
    let rec = store
        .get_record(&site_id, &media_ref)
        .await
        .unwrap()
        .unwrap();
    assert!(rec.is_external);
    assert_eq!(rec.source(), MediaReferenceSource::External);
    assert_eq!(rec.mxc_uri, ext_mxc);

    // 3. Unknown virtual user: returns None
    assert!(
        configured_reconciler
            .reconcile_virtual_user_profile(&site_id, "@_cumments_vu-site_unknown:hs")
            .await
            .unwrap()
            .is_none()
    );

    // 4. Idempotency: repeated call returns identical MediaReference
    let repeated_ref = configured_reconciler
        .reconcile_virtual_user_profile(&site_id, &virtual_user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(media_ref, repeated_ref);
}
