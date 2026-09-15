use std::sync::Arc;

use cumments_core::media_reference::{
    ExternalAvatarReconciler, MediaReference, MediaReferenceSource,
};
use cumments_core::models::SiteId;
use cumments_core::ports::{MediaReferenceResolver, MediaReferenceStore};
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-media-test-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn basic_mapping_and_idempotent_lookup() {
    let store = DbStore::connect(&test_db_url("basic_mapping"))
        .await
        .expect("connect db");
    let site = SiteId::from("blog");
    let mxc = "mxc://homeserver.example.org/asset-12345";

    // Initially, find_reference returns None
    let initial = store.find_reference(&site, mxc).await.unwrap();
    assert!(initial.is_none());

    // First get_or_create allocates a new MediaReference
    let ref1 = store
        .get_or_create_reference(&site, mxc, MediaReferenceSource::Cumments)
        .await
        .unwrap();
    assert!(ref1.as_str().starts_with("cumments-media:"));

    // find_reference now returns ref1
    let found = store.find_reference(&site, mxc).await.unwrap();
    assert_eq!(found.as_ref(), Some(&ref1));

    // Repeated get_or_create returns identical MediaReference
    let ref2 = store
        .get_or_create_reference(&site, mxc, MediaReferenceSource::Cumments)
        .await
        .unwrap();
    assert_eq!(ref1, ref2);

    // Repeated call with External source still returns the existing reference and preserves provenance
    let ref3 = store
        .get_or_create_reference(&site, mxc, MediaReferenceSource::External)
        .await
        .unwrap();
    assert_eq!(ref1, ref3);

    let rec = store.get_record(&site, &ref1).await.unwrap().unwrap();
    assert_eq!(rec.source(), MediaReferenceSource::Cumments);
    assert!(!rec.is_external);
}

#[tokio::test]
async fn reverse_lookup_resolves_original_mxc() {
    let store = DbStore::connect(&test_db_url("reverse_lookup"))
        .await
        .expect("connect db");
    let site = SiteId::from("blog");
    let mxc = "mxc://homeserver.example.org/avatar-abc";

    let reference = store
        .get_or_create_reference(&site, mxc, MediaReferenceSource::Cumments)
        .await
        .unwrap();

    // Reverse lookup via resolve_mxc
    let resolved = store.resolve_mxc(&site, &reference).await.unwrap();
    assert_eq!(resolved.as_deref(), Some(mxc));

    // Full record lookup
    let record = store.get_record(&site, &reference).await.unwrap().unwrap();
    assert_eq!(record.media_reference, reference);
    assert_eq!(record.site_id, site);
    assert_eq!(record.mxc_uri, mxc);
    assert!(!record.is_external);

    // Record lookup by MXC
    let record_by_mxc = store.get_record_by_mxc(&site, mxc).await.unwrap().unwrap();
    assert_eq!(record_by_mxc.media_reference, reference);
    assert_eq!(record_by_mxc.site_id, site);
    assert_eq!(record_by_mxc.mxc_uri, mxc);
}

#[tokio::test]
async fn site_isolation_creates_independent_mappings() {
    let store = DbStore::connect(&test_db_url("site_isolation"))
        .await
        .expect("connect db");
    let site_a = SiteId::from("site-alpha");
    let site_b = SiteId::from("site-beta");
    let shared_mxc = "mxc://matrix.org/shared-media-object";

    let ref_a = store
        .get_or_create_reference(&site_a, shared_mxc, MediaReferenceSource::Cumments)
        .await
        .unwrap();
    let ref_b = store
        .get_or_create_reference(&site_b, shared_mxc, MediaReferenceSource::Cumments)
        .await
        .unwrap();

    // The two sites must receive independent MediaReference identifiers for the same MXC
    assert_ne!(
        ref_a, ref_b,
        "media references must be isolated and independent across different sites"
    );

    // Resolving ref_a under site_a succeeds, but under site_b returns None
    assert_eq!(
        store.resolve_mxc(&site_a, &ref_a).await.unwrap().as_deref(),
        Some(shared_mxc)
    );
    assert_eq!(
        store.resolve_mxc(&site_b, &ref_a).await.unwrap(),
        None,
        "site B must not resolve site A's media reference"
    );

    // Resolving ref_b under site_b succeeds, but under site_a returns None
    assert_eq!(
        store.resolve_mxc(&site_b, &ref_b).await.unwrap().as_deref(),
        Some(shared_mxc)
    );
    assert_eq!(
        store.resolve_mxc(&site_a, &ref_b).await.unwrap(),
        None,
        "site A must not resolve site B's media reference"
    );
}

#[tokio::test]
async fn concurrent_get_or_create_converges_to_single_mapping() {
    let db_url = test_db_url("concurrent_creation");
    let store1 = Arc::new(DbStore::connect(&db_url).await.expect("connect store 1"));
    let store2 = Arc::new(DbStore::connect(&db_url).await.expect("connect store 2"));

    let site = SiteId::from("blog");
    let mxc = "mxc://matrix.org/concurrent-media-race";

    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let store1_task = store1.clone();
    let site1 = site.clone();
    let barrier1 = barrier.clone();
    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;
        store1_task
            .get_or_create_reference(&site1, mxc, MediaReferenceSource::Cumments)
            .await
    });

    let store2_task = store2.clone();
    let site2 = site.clone();
    let barrier2 = barrier.clone();
    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;
        store2_task
            .get_or_create_reference(&site2, mxc, MediaReferenceSource::Cumments)
            .await
    });

    let res1 = handle1.await.unwrap().unwrap();
    let res2 = handle2.await.unwrap().unwrap();

    // Both concurrent callers must converge on the exact same MediaReference
    assert_eq!(
        res1, res2,
        "concurrent get_or_create calls must converge to identical MediaReference"
    );

    // Verify exactly one record exists in database
    let found = store1.find_reference(&site, mxc).await.unwrap().unwrap();
    assert_eq!(found, res1);
}

#[tokio::test]
async fn restart_durability_preserves_media_identity() {
    let db_url = test_db_url("restart_durability");
    let site = SiteId::from("blog");
    let mxc = "mxc://homeserver.org/durable-avatar";

    let initial_ref = {
        let store = DbStore::connect(&db_url).await.expect("connect session 1");
        store
            .get_or_create_reference(&site, mxc, MediaReferenceSource::External)
            .await
            .unwrap()
    };

    // Reopen database with fresh connection pool
    {
        let store = DbStore::connect(&db_url).await.expect("connect session 2");
        let reloaded_ref = store
            .get_or_create_reference(&site, mxc, MediaReferenceSource::External)
            .await
            .unwrap();

        assert_eq!(
            initial_ref, reloaded_ref,
            "media reference identity must survive store restart"
        );

        let resolved = store.resolve_mxc(&site, &reloaded_ref).await.unwrap();
        assert_eq!(resolved.as_deref(), Some(mxc));

        let record = store
            .get_record(&site, &reloaded_ref)
            .await
            .unwrap()
            .unwrap();
        assert!(record.is_external);
    }
}

#[tokio::test]
async fn external_discovery_reconciles_and_preserves_provenance() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("external_discovery"))
            .await
            .expect("connect db"),
    );
    let reconciler = ExternalAvatarReconciler::new(store.clone());

    let site = SiteId::from("blog");
    let external_mxc = "mxc://homeserver.org/external-avatar-999";

    // 1. Reconcile explicit external avatar
    let ref1 = reconciler
        .reconcile_external_avatar(&site, external_mxc)
        .await
        .unwrap();

    let record1 = store.get_record(&site, &ref1).await.unwrap().unwrap();
    assert!(
        record1.is_external,
        "externally discovered avatar must be marked is_external = true"
    );
    assert_eq!(record1.source(), MediaReferenceSource::External);

    // 2. Repeated discovery reuses existing mapping
    let ref2 = reconciler
        .reconcile_global_profile_avatar(&site, Some(external_mxc))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ref1, ref2);

    // 3. If an upload was created first (is_external = false), external discovery does NOT flip it
    let upload_mxc = "mxc://homeserver.org/upload-avatar-111";
    let upload_ref = store
        .get_or_create_reference(&site, upload_mxc, MediaReferenceSource::Cumments)
        .await
        .unwrap();

    let upload_record = store.get_record(&site, &upload_ref).await.unwrap().unwrap();
    assert!(!upload_record.is_external);
    assert_eq!(upload_record.source(), MediaReferenceSource::Cumments);

    // Subsequent external reconciliation of the same MXC returns existing ref and preserves is_external = false
    let reconciled_upload = reconciler
        .reconcile_avatar(&site, upload_mxc)
        .await
        .unwrap();
    assert_eq!(upload_ref, reconciled_upload);

    let preserved_record = store.get_record(&site, &upload_ref).await.unwrap().unwrap();
    assert!(
        !preserved_record.is_external,
        "existing is_external provenance must not be arbitrarily flipped"
    );
    assert_eq!(preserved_record.source(), MediaReferenceSource::Cumments);

    // 4. Missing or empty avatars return Ok(None)
    assert!(
        reconciler
            .reconcile_global_profile_avatar(&site, None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        reconciler
            .reconcile_global_profile_avatar(&site, Some(""))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        reconciler
            .reconcile_global_profile_avatar(&site, Some("https://example.com/not-mxc"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn missing_resolution_is_read_only_and_returns_none() {
    let store = DbStore::connect(&test_db_url("missing_resolution"))
        .await
        .expect("connect db");
    let site = SiteId::from("blog");

    let unmapped_ref = MediaReference::new_v4();

    // resolve_mxc must return Ok(None) and NOT allocate a mapping
    let resolved = store.resolve_mxc(&site, &unmapped_ref).await.unwrap();
    assert!(resolved.is_none());

    let record = store.get_record(&site, &unmapped_ref).await.unwrap();
    assert!(record.is_none());

    // find_reference on unmapped MXC returns Ok(None)
    let found = store
        .find_reference(&site, "mxc://example.org/never-mapped")
        .await
        .unwrap();
    assert!(found.is_none());
}

#[tokio::test]
async fn ownership_separation_never_creates_media_uploads_record() {
    use cumments_core::ports::MessageStore;

    let store = Arc::new(
        DbStore::connect(&test_db_url("ownership_separation"))
            .await
            .expect("connect db"),
    );
    let reconciler = ExternalAvatarReconciler::new(store.clone());

    let site = SiteId::from("blog");
    let external_mxc = "mxc://homeserver.org/external-avatar-pure";

    // Verify initial unused media list is empty
    let initial_unused = store
        .list_media_upload_candidates_before(chrono::Utc::now() + chrono::Duration::hours(1))
        .await
        .unwrap();
    assert!(initial_unused.is_empty());

    // Reconcile external avatar
    let media_ref = reconciler
        .reconcile_avatar(&site, external_mxc)
        .await
        .unwrap();
    assert!(media_ref.as_str().starts_with("cumments-media:"));

    // Verify media_references has the record
    let record = store.get_record(&site, &media_ref).await.unwrap().unwrap();
    assert_eq!(record.mxc_uri, external_mxc);
    assert!(record.is_external);

    // CRITICAL: media_uploads must NOT contain any record for this external avatar!
    let post_unused = store
        .list_media_upload_candidates_before(chrono::Utc::now() + chrono::Duration::hours(1))
        .await
        .unwrap();
    assert!(
        post_unused.is_empty(),
        "external avatar discovery must never create or touch media_uploads ownership records"
    );
}

#[tokio::test]
async fn profile_executor_integration_with_media_reference_store() {
    use cumments_core::ports::ProfileStore;
    use cumments_core::profile::{
        ProfileOperationExecutionResult, ProfileOperationExecutor, ProfileOperationStatus,
        ProfileTargetValue,
    };
    use cumments_test_utils::TestDriver;

    let store = Arc::new(
        DbStore::connect(&test_db_url("executor_store_integration"))
            .await
            .expect("connect db"),
    );
    let driver = Arc::new(TestDriver::new());

    // Use DbStore itself as the MediaReferenceResolver!
    let executor = ProfileOperationExecutor::new(
        store.clone(),
        driver.clone(),
        Some(store.clone() as Arc<dyn MediaReferenceResolver>),
    );

    let site = SiteId::from("blog");
    let author = "pubkey-avatar-integration";

    // 1. Create a media reference mapping in DbStore
    let mxc_url = "mxc://homeserver.example.org/avatar-file-789";
    let media_ref = store
        .get_or_create_reference(&site, mxc_url, MediaReferenceSource::Cumments)
        .await
        .unwrap();

    // 2. Submit a SetAvatar profile operation with this MediaReference
    let op_avatar = ProfileTargetValue::SetAvatar(media_ref.clone());
    store
        .claim_or_get_profile_operation("op-avatar-e2e", author, &site, &op_avatar)
        .await
        .unwrap();

    // 3. Execute: ProfileOperationExecutor resolves MediaReference -> MXC via DbStore and calls driver!
    let res = executor.execute("op-avatar-e2e").await.unwrap();
    assert_eq!(res, ProfileOperationExecutionResult::Completed);

    let op = store
        .get_profile_operation("op-avatar-e2e")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op.status, ProfileOperationStatus::Completed);

    let avatar_calls = driver.set_avatar_calls.lock().await;
    assert_eq!(avatar_calls.len(), 1);
    assert_eq!(avatar_calls[0].0, author);
    assert_eq!(avatar_calls[0].2, mxc_url);
}
