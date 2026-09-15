//! Integration tests for ExternalProfileReconciler in cumments-reconciler.

use cumments_core::media_reference::MediaReference;
use cumments_core::models::{SiteId, VisitorProfile};
use cumments_core::ports::{
    MediaReferenceResolver, MediaReferenceStore, MessageStore, SiteStore, VirtualUserStore,
};
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
async fn profile_reconciler_projects_deterministic_references() {
    let db_url = test_db_url("project_deterministic");
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
    );

    // 1. A Matrix profile avatar projects the deterministic reference.
    let author = "author-pubkey-1";
    let matrix_mxc = "mxc://hs/matrix-avatar-111";
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author.to_string()),
        VisitorProfile {
            display_name: Some("Matrix User".to_string()),
            avatar_url: Some(matrix_mxc.to_string()),
        },
    );

    let media_ref = reconciler
        .reconcile_visitor_profile(&site_id, author)
        .await
        .expect("reconcile profile")
        .expect("media ref exists");

    assert_eq!(media_ref, MediaReference::from_media(&site_id, matrix_mxc));
    let rec = store
        .get_record(&site_id, &media_ref)
        .await
        .expect("get record")
        .expect("record exists");
    assert_eq!(rec.mxc_uri, matrix_mxc);
    assert_eq!(
        store
            .resolve_mxc(&site_id, &media_ref)
            .await
            .expect("resolve mxc")
            .as_deref(),
        Some(matrix_mxc)
    );

    // 2. Idempotency: repeated observation reuses the exact same reference.
    let media_ref_repeated = reconciler
        .reconcile_visitor_profile(&site_id, author)
        .await
        .expect("repeat reconcile")
        .expect("media ref exists");
    assert_eq!(media_ref, media_ref_repeated);

    // 3. A Cumments-owned upload follows the same identity path; ownership
    // evidence is a separate concern and is untouched.
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
    assert_eq!(
        cumments_ref,
        MediaReference::from_media(&site_id, cumments_mxc)
    );
    assert!(
        store
            .media_upload_owned_by(cumments_mxc, author_cumments, site_id.as_str(), "post-1")
            .await
            .expect("ownership check"),
        "projection must not disturb upload ownership evidence"
    );

    // The observed Matrix avatar did not create ownership evidence.
    assert!(
        !store
            .has_media_upload_for_site(site_id.as_str(), matrix_mxc)
            .await
            .expect("ownership check")
    );

    // 4. Profiles without an avatar and unknown authors return None.
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
    assert!(
        reconciler
            .reconcile_visitor_profile(&site_id, "unknown-author")
            .await
            .unwrap()
            .is_none()
    );

    // 5. Restart durability: a new reconciler instance on the same DB returns
    // the same deterministic reference.
    let driver_v2 = Arc::new(TestDriver::new());
    driver_v2.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author.to_string()),
        VisitorProfile {
            display_name: Some("Matrix User".to_string()),
            avatar_url: Some(matrix_mxc.to_string()),
        },
    );
    let reconciler_v2 =
        ExternalProfileReconciler::new(driver_v2, store.clone() as Arc<dyn MediaReferenceStore>);
    let restarted_ref = reconciler_v2
        .reconcile_visitor_profile(&site_id, author)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(media_ref, restarted_ref);
}

#[tokio::test]
async fn profile_reconciler_reconciles_virtual_user_profile() {
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
    let mxc = "mxc://hs/virtual-user-avatar-999";
    driver.visitor_profiles.lock().await.insert(
        (site_id.as_str().to_string(), author_pubkey.to_string()),
        VisitorProfile {
            display_name: Some("Virtual User".to_string()),
            avatar_url: Some(mxc.to_string()),
        },
    );

    // 1. Without VirtualUserStore wired: returns None.
    let unconfigured_reconciler = ExternalProfileReconciler::new(
        driver.clone(),
        store.clone() as Arc<dyn MediaReferenceStore>,
    );
    assert!(
        unconfigured_reconciler
            .reconcile_virtual_user_profile(&site_id, &virtual_user_id)
            .await
            .unwrap()
            .is_none()
    );

    // 2. With VirtualUserStore wired: projects the deterministic reference.
    let configured_reconciler =
        unconfigured_reconciler.with_virtual_user_store(store.clone() as Arc<dyn VirtualUserStore>);

    let media_ref = configured_reconciler
        .reconcile_virtual_user_profile(&site_id, &virtual_user_id)
        .await
        .expect("reconcile virtual user profile")
        .expect("media ref resolved");
    assert_eq!(media_ref, MediaReference::from_media(&site_id, mxc));

    let rec = store
        .get_record(&site_id, &media_ref)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rec.mxc_uri, mxc);

    // 3. Unknown virtual user: returns None.
    assert!(
        configured_reconciler
            .reconcile_virtual_user_profile(&site_id, "@_cumments_vu-site_unknown:hs")
            .await
            .unwrap()
            .is_none()
    );

    // 4. Idempotency: repeated call returns identical MediaReference.
    let repeated_ref = configured_reconciler
        .reconcile_virtual_user_profile(&site_id, &virtual_user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(media_ref, repeated_ref);
}
