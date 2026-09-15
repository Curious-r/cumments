//! Integration tests for ProfileOperationsPass in cumments-reconciler.
//!
//! Tests verify durable asynchronous queue progression, serialization invariants,
//! crash recovery, and multi-worker safety:
//! 1. Automatic progression: pending successor progresses automatically once predecessor reaches terminal state.
//! 2. Failure predecessor: deterministic failure still allows subsequent operations for that field to proceed.
//! 3. Unknown barrier: ambiguous downstream failure blocks subsequent operations for that field indefinitely.
//! 4. Restart durability: durable pending operations execute and progress across process restarts.
//! 5. Multiple fields progress independently: display name and avatar queues do not block each other.
//! 6. Duplicate workers: concurrent passes prevent double dispatch via atomic execution leases.

use std::sync::Arc;

use cumments_core::models::SiteId;
use cumments_core::ports::{ProfileStore, SiteStore};
use cumments_core::profile::{
    ProfileClaimOutcome, ProfileDriverError, ProfileOperationStatus, ProfileTargetValue,
};
use cumments_reconciler::ProfileOperationsPass;
use cumments_store::DbStore;
use cumments_test_utils::TestDriver;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-reconciler-ops-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn automatic_progression_after_terminal_predecessor() {
    let db_url = test_db_url("auto_progression");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let site = SiteId::from("my-site");
    store
        .ensure_site_exists(site.as_str(), "!space:hs")
        .await
        .expect("ensure site");

    let driver = Arc::new(TestDriver::new());
    let pass = ProfileOperationsPass::for_test(store.clone(), driver.clone());

    let author = "pubkey-visitor-1";
    let target_a = ProfileTargetValue::SetDisplayName("Alice".to_string());
    let target_b = ProfileTargetValue::SetDisplayName("Bob".to_string());

    // Enqueue op-a and op-b both in Pending status
    let outcome_a = store
        .claim_or_get_profile_operation("op-a", author, &site, &target_a)
        .await
        .unwrap();
    let outcome_b = store
        .claim_or_get_profile_operation("op-b", author, &site, &target_b)
        .await
        .unwrap();
    assert!(matches!(outcome_a, ProfileClaimOutcome::New(_)));
    assert!(matches!(outcome_b, ProfileClaimOutcome::New(_)));

    assert_eq!(
        store
            .get_profile_operation("op-a")
            .await
            .unwrap()
            .unwrap()
            .status,
        ProfileOperationStatus::Pending
    );
    assert_eq!(
        store
            .get_profile_operation("op-b")
            .await
            .unwrap()
            .unwrap()
            .status,
        ProfileOperationStatus::Pending
    );

    // Run pass once. Both Op A and Op B must execute and reach Completed automatically
    // without any intermediate client API calls.
    let handled = pass.reconcile().await.expect("reconcile pass");
    assert_eq!(handled, 2);

    let op_a = store.get_profile_operation("op-a").await.unwrap().unwrap();
    let op_b = store.get_profile_operation("op-b").await.unwrap().unwrap();
    assert_eq!(op_a.status, ProfileOperationStatus::Completed);
    assert_eq!(op_b.status, ProfileOperationStatus::Completed);

    // Verify driver received both calls in correct sequence
    let calls = driver.set_display_name_calls.lock().await;
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0],
        (author.to_string(), site.clone(), "Alice".to_string())
    );
    assert_eq!(
        calls[1],
        (author.to_string(), site.clone(), "Bob".to_string())
    );
}

#[tokio::test]
async fn failure_predecessor_allows_subsequent_to_proceed() {
    let db_url = test_db_url("failure_predecessor");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let site = SiteId::from("my-site");
    store
        .ensure_site_exists(site.as_str(), "!space:hs")
        .await
        .expect("ensure site");

    let driver = Arc::new(TestDriver::new());
    let pass = ProfileOperationsPass::for_test(store.clone(), driver.clone());

    let author = "pubkey-visitor-failure";
    let target_a = ProfileTargetValue::SetDisplayName("BadName".to_string());
    let target_b = ProfileTargetValue::SetDisplayName("GoodName".to_string());

    store
        .claim_or_get_profile_operation("op-fail", author, &site, &target_a)
        .await
        .unwrap();
    store
        .claim_or_get_profile_operation("op-succ", author, &site, &target_b)
        .await
        .unwrap();

    // Configure the driver to fail op-fail deterministically
    driver
        .set_next_profile_error(ProfileDriverError::Deterministic(
            "invalid display name".into(),
        ))
        .await;

    let handled = pass.reconcile().await.expect("reconcile pass");
    assert_eq!(handled, 2);

    let op_fail = store
        .get_profile_operation("op-fail")
        .await
        .unwrap()
        .unwrap();
    let op_succ = store
        .get_profile_operation("op-succ")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op_fail.status, ProfileOperationStatus::Failed);
    assert_eq!(op_succ.status, ProfileOperationStatus::Completed);

    // Driver recorded call for op-succ (op-fail returned error before recording)
    let calls = driver.set_display_name_calls.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].2, "GoodName");
}

#[tokio::test]
async fn unknown_predecessor_blocks_subsequent_indefinitely() {
    let db_url = test_db_url("unknown_barrier");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let site = SiteId::from("my-site");
    store
        .ensure_site_exists(site.as_str(), "!space:hs")
        .await
        .expect("ensure site");

    let driver = Arc::new(TestDriver::new());
    let pass = ProfileOperationsPass::for_test(store.clone(), driver.clone());

    let author = "pubkey-visitor-unknown";
    let target_a = ProfileTargetValue::SetDisplayName("TimeoutName".to_string());
    let target_b = ProfileTargetValue::SetDisplayName("BlockedName".to_string());

    store
        .claim_or_get_profile_operation("op-unknown", author, &site, &target_a)
        .await
        .unwrap();
    store
        .claim_or_get_profile_operation("op-blocked", author, &site, &target_b)
        .await
        .unwrap();

    // Set ambiguous error (timeout) on op-unknown
    driver
        .set_next_profile_error(ProfileDriverError::Ambiguous("upstream timed out".into()))
        .await;

    let handled = pass.reconcile().await.expect("reconcile pass");
    assert_eq!(handled, 1);

    let op_unk = store
        .get_profile_operation("op-unknown")
        .await
        .unwrap()
        .unwrap();
    let op_blk = store
        .get_profile_operation("op-blocked")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op_unk.status, ProfileOperationStatus::Unknown);
    assert_eq!(op_blk.status, ProfileOperationStatus::Pending);

    // Driver has 0 recorded successful calls (op-unknown errored, op-blocked never ran)
    {
        let calls = driver.set_display_name_calls.lock().await;
        assert_eq!(calls.len(), 0);
    }

    // Repeated reconcile runs do NOT progress op-blocked; it remains blocked by Unknown indefinitely
    let handled_repeat = pass.reconcile().await.expect("repeat reconcile");
    assert_eq!(handled_repeat, 0);

    let op_blk_after = store
        .get_profile_operation("op-blocked")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op_blk_after.status, ProfileOperationStatus::Pending);

    let calls_after = driver.set_display_name_calls.lock().await;
    assert_eq!(calls_after.len(), 0);
}

#[tokio::test]
async fn restart_durability() {
    let db_url = test_db_url("restart_durability");
    let site = SiteId::from("my-site");
    let author = "pubkey-visitor-restart";

    // Session 1: Enqueue Op 1 and Op 2.
    // Op 1 completes, Op 2 is left pending.
    {
        let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
        store
            .ensure_site_exists(site.as_str(), "!space:hs")
            .await
            .expect("ensure site");

        let driver = Arc::new(TestDriver::new());
        let pass = ProfileOperationsPass::for_test(store.clone(), driver.clone());

        let target_1 = ProfileTargetValue::SetDisplayName("First".to_string());
        let target_2 = ProfileTargetValue::SetDisplayName("Second".to_string());

        store
            .claim_or_get_profile_operation("op-1", author, &site, &target_1)
            .await
            .unwrap();
        assert_eq!(pass.reconcile().await.unwrap(), 1);
        assert_eq!(
            store
                .get_profile_operation("op-1")
                .await
                .unwrap()
                .unwrap()
                .status,
            ProfileOperationStatus::Completed
        );

        // Enqueue op-2 while system is running, then simulate process crash / restart
        store
            .claim_or_get_profile_operation("op-2", author, &site, &target_2)
            .await
            .unwrap();
        assert_eq!(
            store
                .get_profile_operation("op-2")
                .await
                .unwrap()
                .unwrap()
                .status,
            ProfileOperationStatus::Pending
        );
    }

    // Session 2: Fresh process startup
    {
        let store = Arc::new(DbStore::connect(&db_url).await.expect("reconnect db"));
        // Recover crashed dispatching operations at startup
        store.recover_crashed_dispatching().await.expect("recover");

        let driver = Arc::new(TestDriver::new());
        let pass = ProfileOperationsPass::for_test(store.clone(), driver.clone());

        // Reconciler runs and picks up op-2
        let handled = pass.reconcile().await.expect("reconcile on restart");
        assert_eq!(handled, 1);

        let op_2 = store.get_profile_operation("op-2").await.unwrap().unwrap();
        assert_eq!(op_2.status, ProfileOperationStatus::Completed);

        let calls = driver.set_display_name_calls.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].2, "Second");
    }
}

#[tokio::test]
async fn multiple_fields_progress_independently() {
    let db_url = test_db_url("multiple_fields");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let site = SiteId::from("my-site");
    store
        .ensure_site_exists(site.as_str(), "!space:hs")
        .await
        .expect("ensure site");

    let driver = Arc::new(TestDriver::new());
    let pass = ProfileOperationsPass::for_test(store.clone(), driver.clone());

    let author = "pubkey-visitor-multi";
    let name_target = ProfileTargetValue::SetDisplayName("Alice".to_string());
    let avatar_target = ProfileTargetValue::SetAvatar("mxc://hs/avatar1".to_string());

    // Enqueue DisplayName and Avatar operations
    store
        .claim_or_get_profile_operation("op-name", author, &site, &name_target)
        .await
        .unwrap();
    store
        .claim_or_get_profile_operation("op-avatar", author, &site, &avatar_target)
        .await
        .unwrap();

    // Make the DisplayName fail with Ambiguous (Unknown)
    driver
        .set_next_profile_error(ProfileDriverError::Ambiguous("name service timeout".into()))
        .await;

    // Run pass: op-name becomes Unknown, op-avatar completes
    let handled = pass.reconcile().await.expect("reconcile pass");
    assert_eq!(handled, 2);

    let op_name = store
        .get_profile_operation("op-name")
        .await
        .unwrap()
        .unwrap();
    let op_avatar = store
        .get_profile_operation("op-avatar")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op_name.status, ProfileOperationStatus::Unknown);
    assert_eq!(op_avatar.status, ProfileOperationStatus::Completed);

    // Enqueue second avatar operation
    let clear_avatar_target = ProfileTargetValue::ClearAvatar;
    store
        .claim_or_get_profile_operation("op-avatar-2", author, &site, &clear_avatar_target)
        .await
        .unwrap();

    // Reconcile again: second avatar operation progresses and completes, even though DisplayName is Unknown!
    let handled_2 = pass.reconcile().await.expect("reconcile pass 2");
    assert_eq!(handled_2, 1);

    let op_avatar_2 = store
        .get_profile_operation("op-avatar-2")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op_avatar_2.status, ProfileOperationStatus::Completed);

    // And verify DisplayName is still blocked if we add a second display name operation
    let name_target_2 = ProfileTargetValue::SetDisplayName("Alice2".to_string());
    store
        .claim_or_get_profile_operation("op-name-2", author, &site, &name_target_2)
        .await
        .unwrap();

    let handled_3 = pass.reconcile().await.expect("reconcile pass 3");
    assert_eq!(handled_3, 0);
    assert_eq!(
        store
            .get_profile_operation("op-name-2")
            .await
            .unwrap()
            .unwrap()
            .status,
        ProfileOperationStatus::Pending
    );
}

#[tokio::test]
async fn duplicate_workers_prevent_double_dispatch() {
    let db_url = test_db_url("concurrent_workers");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let site = SiteId::from("my-site");
    store
        .ensure_site_exists(site.as_str(), "!space:hs")
        .await
        .expect("ensure site");

    let driver = Arc::new(TestDriver::new());

    // Worker 1 and Worker 2 share the same store and driver
    let pass1 = Arc::new(ProfileOperationsPass::for_test(
        store.clone(),
        driver.clone(),
    ));
    let pass2 = Arc::new(ProfileOperationsPass::for_test(
        store.clone(),
        driver.clone(),
    ));

    let author_a = "pubkey-a";
    let author_b = "pubkey-b";
    let target_a = ProfileTargetValue::SetDisplayName("Alice".to_string());
    let target_b = ProfileTargetValue::SetDisplayName("Bob".to_string());

    store
        .claim_or_get_profile_operation("op-a", author_a, &site, &target_a)
        .await
        .unwrap();
    store
        .claim_or_get_profile_operation("op-b", author_b, &site, &target_b)
        .await
        .unwrap();

    // Concurrently run pass1 and pass2
    let p1 = pass1.clone();
    let p2 = pass2.clone();
    let (res1, res2) = tokio::join!(
        tokio::spawn(async move { p1.reconcile().await }),
        tokio::spawn(async move { p2.reconcile().await }),
    );

    let handled1 = res1.unwrap().expect("p1 reconcile");
    let handled2 = res2.unwrap().expect("p2 reconcile");

    // Total handled operations across both workers must equal 2
    assert_eq!(handled1 + handled2, 2);

    assert_eq!(
        store
            .get_profile_operation("op-a")
            .await
            .unwrap()
            .unwrap()
            .status,
        ProfileOperationStatus::Completed
    );
    assert_eq!(
        store
            .get_profile_operation("op-b")
            .await
            .unwrap()
            .unwrap()
            .status,
        ProfileOperationStatus::Completed
    );

    // Driver received exactly one call for op-a and one for op-b (no duplicate execution)
    let calls = driver.set_display_name_calls.lock().await;
    assert_eq!(calls.len(), 2);
    let mut names: Vec<String> = calls.iter().map(|c| c.2.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["Alice".to_string(), "Bob".to_string()]);
}

#[tokio::test]
async fn worker_sweep_respects_site_scope_when_another_site_is_blocked() {
    // Case 6: Worker sweep: site-A blocked (Unknown), site-B executable (Pending)
    // worker sweep must execute site-B without being affected by site-A blocker.
    let db_url = test_db_url("worker_sweep_site_scope");
    let store = Arc::new(DbStore::connect(&db_url).await.expect("connect db"));
    let site_a = SiteId::from("site-a");
    let site_b = SiteId::from("site-b");
    store
        .ensure_site_exists(site_a.as_str(), "!space-a:hs")
        .await
        .expect("ensure site a");
    store
        .ensure_site_exists(site_b.as_str(), "!space-b:hs")
        .await
        .expect("ensure site b");

    let driver = Arc::new(TestDriver::new());
    let pass = ProfileOperationsPass::for_test(store.clone(), driver.clone());

    let author = "pubkey-sweep-shared";
    let target_a1 = ProfileTargetValue::SetDisplayName("Name A1".to_string());
    let target_a2 = ProfileTargetValue::SetDisplayName("Name A2".to_string());
    let target_b1 = ProfileTargetValue::SetDisplayName("Name B1".to_string());

    // 1. Enqueue op-a1 on site-a
    store
        .claim_or_get_profile_operation("op-a1", author, &site_a, &target_a1)
        .await
        .unwrap();

    // Make op-a1 fail ambiguously -> transitions to Unknown
    driver
        .set_next_profile_error(ProfileDriverError::Ambiguous("site A timeout".into()))
        .await;
    assert_eq!(pass.reconcile().await.unwrap(), 1);
    assert_eq!(
        store
            .get_profile_operation("op-a1")
            .await
            .unwrap()
            .unwrap()
            .status,
        ProfileOperationStatus::Unknown
    );

    // 2. Enqueue op-a2 on site-a (which is now blocked by op-a1 being Unknown)
    store
        .claim_or_get_profile_operation("op-a2", author, &site_a, &target_a2)
        .await
        .unwrap();

    // 3. Enqueue op-b1 on site-b for the SAME author and field
    store
        .claim_or_get_profile_operation("op-b1", author, &site_b, &target_b1)
        .await
        .unwrap();

    // Both op-a2 and op-b1 are currently Pending
    assert_eq!(
        store
            .get_profile_operation("op-a2")
            .await
            .unwrap()
            .unwrap()
            .status,
        ProfileOperationStatus::Pending
    );
    assert_eq!(
        store
            .get_profile_operation("op-b1")
            .await
            .unwrap()
            .unwrap()
            .status,
        ProfileOperationStatus::Pending
    );

    // 4. Run reconciler pass sweep!
    // op-b1 MUST be executed and reach Completed!
    // op-a2 MUST remain Pending and NOT block op-b1!
    let handled = pass.reconcile().await.expect("sweep reconcile");
    assert_eq!(handled, 1);

    assert_eq!(
        store
            .get_profile_operation("op-b1")
            .await
            .unwrap()
            .unwrap()
            .status,
        ProfileOperationStatus::Completed
    );
    assert_eq!(
        store
            .get_profile_operation("op-a2")
            .await
            .unwrap()
            .unwrap()
            .status,
        ProfileOperationStatus::Pending
    );

    // Verify driver only processed op-b1 on site-b
    let calls = driver.set_display_name_calls.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        (author.to_string(), site_b.clone(), "Name B1".to_string())
    );
}
