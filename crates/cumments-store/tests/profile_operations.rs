use std::sync::Arc;

use cumments_core::media_reference::MediaReference;
use cumments_core::models::SiteId;
use cumments_core::ports::{MediaReferenceResolver, ProfileStore};
use cumments_core::profile::{
    ProfileClaimOutcome, ProfileDriverError, ProfileField, ProfileOperationExecutionResult,
    ProfileOperationExecutor, ProfileOperationStatus, ProfileTargetValue,
};
use cumments_store::DbStore;
use cumments_test_utils::TestDriver;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-profile-test-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

struct MockMediaResolver;

#[async_trait::async_trait]
impl MediaReferenceResolver for MockMediaResolver {
    async fn resolve_mxc(
        &self,
        site_id: &SiteId,
        reference: &MediaReference,
    ) -> anyhow::Result<Option<String>> {
        Ok(Some(format!(
            "mxc://example.com/{}/{}",
            site_id.as_str(),
            reference.uuid()
        )))
    }
}

#[tokio::test]
async fn same_field_queueing_order_and_blocking() {
    let store = DbStore::connect(&test_db_url("queueing"))
        .await
        .expect("connect db");
    let site = SiteId::from("blog");
    let author = "pubkey-visitor-1";

    let op1_target = ProfileTargetValue::SetDisplayName("Alice".to_string());
    let op2_target = ProfileTargetValue::SetDisplayName("Bob".to_string());
    let op3_target = ProfileTargetValue::ClearDisplayName;

    let res1 = store
        .claim_or_get_profile_operation("op-1", author, &site, &op1_target)
        .await
        .expect("claim op1");
    let ProfileClaimOutcome::New(op1) = res1 else {
        panic!("expected New, got {res1:?}");
    };
    assert_eq!(op1.sequence, 1);
    assert_eq!(op1.status, ProfileOperationStatus::Pending);

    let res2 = store
        .claim_or_get_profile_operation("op-2", author, &site, &op2_target)
        .await
        .expect("claim op2");
    let ProfileClaimOutcome::New(op2) = res2 else {
        panic!("expected New, got {res2:?}");
    };
    assert_eq!(op2.sequence, 2);
    assert_eq!(op2.status, ProfileOperationStatus::Pending);

    let res3 = store
        .claim_or_get_profile_operation("op-3", author, &site, &op3_target)
        .await
        .expect("claim op3");
    let ProfileClaimOutcome::New(op3) = res3 else {
        panic!("expected New, got {res3:?}");
    };
    assert_eq!(op3.sequence, 3);
    assert_eq!(op3.status, ProfileOperationStatus::Pending);

    // op2 cannot execute before op1
    assert!(!store.claim_for_execution("op-2").await.unwrap());
    assert!(!store.claim_for_execution("op-3").await.unwrap());

    // op1 can execute
    assert!(store.claim_for_execution("op-1").await.unwrap());
    let op1_fetched = store.get_profile_operation("op-1").await.unwrap().unwrap();
    assert_eq!(op1_fetched.status, ProfileOperationStatus::Dispatching);

    // op2 is still blocked while op1 is Dispatching
    assert!(!store.claim_for_execution("op-2").await.unwrap());

    // Complete op1
    store.record_completed("op-1", None).await.unwrap();
    let op1_completed = store.get_profile_operation("op-1").await.unwrap().unwrap();
    assert_eq!(op1_completed.status, ProfileOperationStatus::Completed);
    assert!(op1_completed.resolved_at.is_some());

    // op3 still cannot jump ahead of op2
    assert!(!store.claim_for_execution("op-3").await.unwrap());

    // op2 can now execute
    assert!(store.claim_for_execution("op-2").await.unwrap());
    // Fail op2 deterministically
    store
        .record_failed("op-2", "display name too long")
        .await
        .unwrap();
    let op2_failed = store.get_profile_operation("op-2").await.unwrap().unwrap();
    assert_eq!(op2_failed.status, ProfileOperationStatus::Failed);

    // op3 can now execute
    assert!(store.claim_for_execution("op-3").await.unwrap());
    store.record_completed("op-3", None).await.unwrap();

    let ops = store
        .list_operations_for_field(author, ProfileField::DisplayName)
        .await
        .unwrap();
    assert_eq!(ops.len(), 3);
    assert_eq!(ops[0].operation_id, "op-1");
    assert_eq!(ops[1].operation_id, "op-2");
    assert_eq!(ops[2].operation_id, "op-3");
}

#[tokio::test]
async fn independent_fields_do_not_block_each_other() {
    let store = DbStore::connect(&test_db_url("independent_fields"))
        .await
        .expect("connect db");
    let site = SiteId::from("blog");
    let author = "pubkey-visitor-multi-field";

    let op_name = ProfileTargetValue::SetDisplayName("Alice".to_string());
    let media = MediaReference::new_v4();
    let op_avatar = ProfileTargetValue::SetAvatar(media);

    store
        .claim_or_get_profile_operation("op-name-1", author, &site, &op_name)
        .await
        .unwrap();
    store
        .claim_or_get_profile_operation("op-avatar-1", author, &site, &op_avatar)
        .await
        .unwrap();

    // Both can claim execution concurrently because fields are independent
    assert!(store.claim_for_execution("op-name-1").await.unwrap());
    assert!(store.claim_for_execution("op-avatar-1").await.unwrap());

    let fetched_name = store
        .get_profile_operation("op-name-1")
        .await
        .unwrap()
        .unwrap();
    let fetched_avatar = store
        .get_profile_operation("op-avatar-1")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(fetched_name.status, ProfileOperationStatus::Dispatching);
    assert_eq!(fetched_avatar.status, ProfileOperationStatus::Dispatching);
}

#[tokio::test]
async fn independent_visitors_do_not_block_each_other() {
    let store = DbStore::connect(&test_db_url("independent_visitors"))
        .await
        .expect("connect db");
    let site = SiteId::from("blog");
    let author_a = "pubkey-alice";
    let author_b = "pubkey-bob";

    let op_a = ProfileTargetValue::SetDisplayName("Alice".to_string());
    let op_b = ProfileTargetValue::SetDisplayName("Bob".to_string());

    store
        .claim_or_get_profile_operation("op-a", author_a, &site, &op_a)
        .await
        .unwrap();
    store
        .claim_or_get_profile_operation("op-b", author_b, &site, &op_b)
        .await
        .unwrap();

    assert!(store.claim_for_execution("op-a").await.unwrap());
    assert!(store.claim_for_execution("op-b").await.unwrap());
}

#[tokio::test]
async fn unknown_status_blocks_later_operations_until_resolved() {
    let store = DbStore::connect(&test_db_url("unknown_blocking"))
        .await
        .expect("connect db");
    let site = SiteId::from("blog");
    let author = "pubkey-unknown-test";

    let op1_target = ProfileTargetValue::SetDisplayName("Name 1".to_string());
    let op2_target = ProfileTargetValue::SetDisplayName("Name 2".to_string());

    store
        .claim_or_get_profile_operation("op-1", author, &site, &op1_target)
        .await
        .unwrap();
    store
        .claim_or_get_profile_operation("op-2", author, &site, &op2_target)
        .await
        .unwrap();

    assert!(store.claim_for_execution("op-1").await.unwrap());
    // op1 experiences an ambiguous transport failure
    store
        .record_unknown("op-1", "connection timeout")
        .await
        .unwrap();

    let op1 = store.get_profile_operation("op-1").await.unwrap().unwrap();
    assert_eq!(op1.status, ProfileOperationStatus::Unknown);
    assert!(op1.resolved_at.is_none());

    // op2 cannot execute because op1 is unresolved (Unknown)
    assert!(!store.claim_for_execution("op-2").await.unwrap());
    assert!(
        store
            .get_next_executable_operation(author, ProfileField::DisplayName)
            .await
            .unwrap()
            .is_none()
    );

    // Admin override: explicitly abort op1
    store
        .record_aborted("op-1", "operator intervention")
        .await
        .unwrap();
    let op1_aborted = store.get_profile_operation("op-1").await.unwrap().unwrap();
    assert_eq!(op1_aborted.status, ProfileOperationStatus::Aborted);
    assert!(op1_aborted.resolved_at.is_some());

    // op2 can now execute!
    let next_op = store
        .get_next_executable_operation(author, ProfileField::DisplayName)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next_op.operation_id, "op-2");
    assert!(store.claim_for_execution("op-2").await.unwrap());
}

#[tokio::test]
async fn restart_durability_and_crashed_dispatching_recovery() {
    let url = test_db_url("restart_recovery");
    let site = SiteId::from("blog");
    let author = "pubkey-crash-recovery";

    // Session 1: claim op1 and dispatch, claim op2
    {
        let store = DbStore::connect(&url).await.expect("connect session 1");
        store
            .claim_or_get_profile_operation(
                "op-crashed",
                author,
                &site,
                &ProfileTargetValue::SetDisplayName("Crash".to_string()),
            )
            .await
            .unwrap();
        store
            .claim_or_get_profile_operation(
                "op-waiting",
                author,
                &site,
                &ProfileTargetValue::SetDisplayName("Wait".to_string()),
            )
            .await
            .unwrap();

        assert!(store.claim_for_execution("op-crashed").await.unwrap());
        // Process terminates while op-crashed is Dispatching!
    }

    // Session 2: Server reboots, opens database
    {
        let store = DbStore::connect(&url).await.expect("connect session 2");

        // op-waiting cannot execute because op-crashed is currently in Dispatching
        assert!(!store.claim_for_execution("op-waiting").await.unwrap());

        // Startup recovery sweep runs
        let recovered = store.recover_crashed_dispatching().await.unwrap();
        assert_eq!(recovered, 1);

        // Crashed op transitioned to Unknown
        let op_crashed = store
            .get_profile_operation("op-crashed")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(op_crashed.status, ProfileOperationStatus::Unknown);
        assert_eq!(
            op_crashed.error_detail.as_deref(),
            Some("process crashed during dispatching")
        );

        // op-waiting is still blocked because op-crashed is in Unknown (safety over blind overwrite)
        assert!(!store.claim_for_execution("op-waiting").await.unwrap());
    }
}

#[tokio::test]
async fn idempotent_replay_and_conflict_detection() {
    let store = DbStore::connect(&test_db_url("idempotency"))
        .await
        .expect("connect db");
    let site = SiteId::from("blog");
    let author_alice = "pubkey-alice";
    let author_bob = "pubkey-bob";
    let target_alice = ProfileTargetValue::SetDisplayName("Alice".to_string());

    let res1 = store
        .claim_or_get_profile_operation("op-id-100", author_alice, &site, &target_alice)
        .await
        .unwrap();
    assert!(matches!(res1, ProfileClaimOutcome::New(_)));

    // Exact replay with same operation_id, author, and target_value
    let replay = store
        .claim_or_get_profile_operation("op-id-100", author_alice, &site, &target_alice)
        .await
        .unwrap();
    let ProfileClaimOutcome::Replay(replayed_op) = replay else {
        panic!("expected Replay, got {replay:?}");
    };
    assert_eq!(replayed_op.operation_id, "op-id-100");

    // Conflict: same operation_id with different target_value (different fingerprint)
    let target_different = ProfileTargetValue::SetDisplayName("Different".to_string());
    let conflict1 = store
        .claim_or_get_profile_operation("op-id-100", author_alice, &site, &target_different)
        .await
        .unwrap();
    assert_eq!(conflict1, ProfileClaimOutcome::Conflict);

    // Conflict: same operation_id with different author
    let conflict2 = store
        .claim_or_get_profile_operation("op-id-100", author_bob, &site, &target_alice)
        .await
        .unwrap();
    assert_eq!(conflict2, ProfileClaimOutcome::Conflict);

    // Conflict: same operation_id with different site
    let other_site = SiteId::from("other-site");
    let conflict3 = store
        .claim_or_get_profile_operation("op-id-100", author_alice, &other_site, &target_alice)
        .await
        .unwrap();
    assert_eq!(conflict3, ProfileClaimOutcome::Conflict);
}

#[tokio::test]
async fn profile_operation_executor_end_to_end() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("executor_e2e"))
            .await
            .expect("connect db"),
    );
    let driver = Arc::new(TestDriver::new());
    let resolver = Arc::new(MockMediaResolver);

    let executor = ProfileOperationExecutor::new(
        store.clone(),
        driver.clone(),
        Some(resolver as Arc<dyn MediaReferenceResolver>),
    );

    let site = SiteId::from("blog");
    let author = "pubkey-e2e";

    // 1. Set Display Name
    let op_name = ProfileTargetValue::SetDisplayName("Alice E2E".to_string());
    store
        .claim_or_get_profile_operation("e2e-name", author, &site, &op_name)
        .await
        .unwrap();
    let res = executor.execute("e2e-name").await.unwrap();
    assert_eq!(res, ProfileOperationExecutionResult::Completed);

    let op = store
        .get_profile_operation("e2e-name")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op.status, ProfileOperationStatus::Completed);

    let calls = driver.set_display_name_calls.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, author);
    assert_eq!(calls[0].2, "Alice E2E");
    drop(calls);

    // Re-executing completed operation returns AlreadyProcessed
    let re_exec = executor.execute("e2e-name").await.unwrap();
    assert!(matches!(
        re_exec,
        ProfileOperationExecutionResult::AlreadyProcessed(_)
    ));

    // 2. Clear Display Name
    let op_clear_name = ProfileTargetValue::ClearDisplayName;
    store
        .claim_or_get_profile_operation("e2e-clear-name", author, &site, &op_clear_name)
        .await
        .unwrap();
    let res = executor.execute("e2e-clear-name").await.unwrap();
    assert_eq!(res, ProfileOperationExecutionResult::Completed);
    assert_eq!(driver.clear_display_name_calls.lock().await.len(), 1);

    // 3. Set Avatar
    let media = MediaReference::new_v4();
    let op_avatar = ProfileTargetValue::SetAvatar(media.clone());
    store
        .claim_or_get_profile_operation("e2e-avatar", author, &site, &op_avatar)
        .await
        .unwrap();
    let res = executor.execute("e2e-avatar").await.unwrap();
    assert_eq!(res, ProfileOperationExecutionResult::Completed);

    let avatar_calls = driver.set_avatar_calls.lock().await;
    assert_eq!(avatar_calls.len(), 1);
    assert_eq!(avatar_calls[0].0, author);
    assert!(avatar_calls[0].2.contains(&media.uuid().to_string()));
    drop(avatar_calls);

    // 4. Clear Avatar
    let op_clear_avatar = ProfileTargetValue::ClearAvatar;
    store
        .claim_or_get_profile_operation("e2e-clear-avatar", author, &site, &op_clear_avatar)
        .await
        .unwrap();
    let res = executor.execute("e2e-clear-avatar").await.unwrap();
    assert_eq!(res, ProfileOperationExecutionResult::Completed);
    assert_eq!(driver.clear_avatar_calls.lock().await.len(), 1);
}

#[tokio::test]
async fn executor_deterministic_vs_ambiguous_error_handling() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("executor_errors"))
            .await
            .expect("connect db"),
    );
    let driver = Arc::new(TestDriver::new());
    let executor = ProfileOperationExecutor::new(store.clone(), driver.clone(), None);

    let site = SiteId::from("blog");
    let author = "pubkey-error-test";

    // 1. Inject deterministic error (400 validation error)
    *driver.next_profile_error.lock().await = Some(ProfileDriverError::Deterministic(
        "400 display name contains invalid characters".to_string(),
    ));

    store
        .claim_or_get_profile_operation(
            "op-fail",
            author,
            &site,
            &ProfileTargetValue::SetDisplayName("Bad".to_string()),
        )
        .await
        .unwrap();

    let res_fail = executor.execute("op-fail").await.unwrap();
    assert!(matches!(
        res_fail,
        ProfileOperationExecutionResult::Failed(_)
    ));

    let op_fail = store
        .get_profile_operation("op-fail")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op_fail.status, ProfileOperationStatus::Failed);

    // Because op-fail reached terminal Failed, next operation is NOT blocked
    store
        .claim_or_get_profile_operation(
            "op-ambiguous",
            author,
            &site,
            &ProfileTargetValue::SetDisplayName("Ambiguous".to_string()),
        )
        .await
        .unwrap();

    // 2. Inject ambiguous error (504 gateway timeout)
    *driver.next_profile_error.lock().await = Some(ProfileDriverError::Ambiguous(
        "504 Gateway Timeout".to_string(),
    ));

    let res_amb = executor.execute("op-ambiguous").await.unwrap();
    assert!(matches!(
        res_amb,
        ProfileOperationExecutionResult::Unknown(_)
    ));

    let op_amb = store
        .get_profile_operation("op-ambiguous")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op_amb.status, ProfileOperationStatus::Unknown);

    // 3. Subsequent operation IS blocked because op-ambiguous is Unknown
    store
        .claim_or_get_profile_operation(
            "op-blocked",
            author,
            &site,
            &ProfileTargetValue::SetDisplayName("Next".to_string()),
        )
        .await
        .unwrap();

    let res_blocked = executor.execute("op-blocked").await.unwrap();
    assert!(matches!(
        res_blocked,
        ProfileOperationExecutionResult::Blocked(_)
    ));

    let op_blocked = store
        .get_profile_operation("op-blocked")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op_blocked.status, ProfileOperationStatus::Pending);
}

struct FailingMediaResolver {
    error: Option<String>,
}

#[async_trait::async_trait]
impl MediaReferenceResolver for FailingMediaResolver {
    async fn resolve_mxc(
        &self,
        _site_id: &SiteId,
        _reference: &MediaReference,
    ) -> anyhow::Result<Option<String>> {
        match &self.error {
            Some(err) => anyhow::bail!("{err}"),
            None => Ok(None),
        }
    }
}

#[tokio::test]
async fn resolver_failure_never_strands_operation_in_dispatching() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("resolver_failure_safety"))
            .await
            .expect("connect db"),
    );
    let driver = Arc::new(TestDriver::new());
    let failing_resolver = Arc::new(FailingMediaResolver {
        error: Some("infrastructure error: media backend unreachable".to_string()),
    });

    let executor = ProfileOperationExecutor::new(
        store.clone(),
        driver.clone(),
        Some(failing_resolver as Arc<dyn MediaReferenceResolver>),
    );

    let site = SiteId::from("blog");
    let author = "pubkey-resolver-fail";

    let media1 = MediaReference::new_v4();
    let op_avatar_1 = ProfileTargetValue::SetAvatar(media1);
    store
        .claim_or_get_profile_operation("avatar-fail-1", author, &site, &op_avatar_1)
        .await
        .unwrap();

    let media2 = MediaReference::new_v4();
    let op_avatar_2 = ProfileTargetValue::SetAvatar(media2);
    store
        .claim_or_get_profile_operation("avatar-fail-2", author, &site, &op_avatar_2)
        .await
        .unwrap();

    // Execute op 1 with failing resolver
    let res1 = executor.execute("avatar-fail-1").await.unwrap();
    assert!(
        matches!(res1, ProfileOperationExecutionResult::Unknown(ref msg) if msg.contains("media backend unreachable")),
        "expected Unknown result, got {res1:?}"
    );

    // Matrix driver was NEVER called because resolution failed beforehand
    assert_eq!(driver.set_avatar_calls.lock().await.len(), 0);

    // CRITICAL INVARIANT: Operation must NOT be left in Dispatching! It must be Unknown.
    let op1 = store
        .get_profile_operation("avatar-fail-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op1.status, ProfileOperationStatus::Unknown);
    assert!(op1.resolved_at.is_none());
    assert!(
        op1.error_detail
            .as_deref()
            .unwrap()
            .contains("media backend unreachable")
    );

    // Subsequent operation on the same field MUST be blocked by Unknown status
    let res2 = executor.execute("avatar-fail-2").await.unwrap();
    assert!(
        matches!(res2, ProfileOperationExecutionResult::Blocked(_)),
        "expected Blocked result, got {res2:?}"
    );

    let op2 = store
        .get_profile_operation("avatar-fail-2")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op2.status, ProfileOperationStatus::Pending);
}

#[tokio::test]
async fn resolver_unresolvable_media_records_failed_and_unblocks_next() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("resolver_unresolvable"))
            .await
            .expect("connect db"),
    );
    let driver = Arc::new(TestDriver::new());
    let missing_resolver = Arc::new(FailingMediaResolver { error: None });

    let executor = ProfileOperationExecutor::new(
        store.clone(),
        driver.clone(),
        Some(missing_resolver as Arc<dyn MediaReferenceResolver>),
    );

    let site = SiteId::from("blog");
    let author = "pubkey-resolver-missing";

    let media = MediaReference::new_v4();
    let op_avatar_1 = ProfileTargetValue::SetAvatar(media);
    store
        .claim_or_get_profile_operation("avatar-missing-1", author, &site, &op_avatar_1)
        .await
        .unwrap();

    let op_avatar_2 = ProfileTargetValue::ClearAvatar;
    store
        .claim_or_get_profile_operation("avatar-clear-2", author, &site, &op_avatar_2)
        .await
        .unwrap();

    // Execute op 1: resolver returns Ok(None) -> deterministic business failure (Failed)
    let res1 = executor.execute("avatar-missing-1").await.unwrap();
    assert!(
        matches!(res1, ProfileOperationExecutionResult::Failed(ref msg) if msg.contains("unresolvable")),
        "expected Failed result, got {res1:?}"
    );

    let op1 = store
        .get_profile_operation("avatar-missing-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op1.status, ProfileOperationStatus::Failed);
    assert!(op1.resolved_at.is_some());

    // Because Failed is terminal, subsequent operation can execute successfully!
    let res2 = executor.execute("avatar-clear-2").await.unwrap();
    assert_eq!(res2, ProfileOperationExecutionResult::Completed);

    let op2 = store
        .get_profile_operation("avatar-clear-2")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op2.status, ProfileOperationStatus::Completed);
}

#[tokio::test]
async fn concurrent_same_field_claims_never_produce_two_dispatching() {
    let db_url = test_db_url("concurrent_same_field");
    let store1 = Arc::new(DbStore::connect(&db_url).await.expect("connect store 1"));
    let store2 = Arc::new(DbStore::connect(&db_url).await.expect("connect store 2"));

    let site = SiteId::from("blog");
    let author = "pubkey-concurrent-author";

    let op_a = ProfileTargetValue::SetDisplayName("Alice First".to_string());
    let op_b = ProfileTargetValue::SetDisplayName("Alice Second".to_string());

    store1
        .claim_or_get_profile_operation("op-concurrent-a", author, &site, &op_a)
        .await
        .unwrap();
    store1
        .claim_or_get_profile_operation("op-concurrent-b", author, &site, &op_b)
        .await
        .unwrap();

    // Verify initial state: both are Pending
    let a_init = store1
        .get_profile_operation("op-concurrent-a")
        .await
        .unwrap()
        .unwrap();
    let b_init = store1
        .get_profile_operation("op-concurrent-b")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a_init.status, ProfileOperationStatus::Pending);
    assert_eq!(b_init.status, ProfileOperationStatus::Pending);
    assert_eq!(a_init.sequence, 1);
    assert_eq!(b_init.sequence, 2);

    // Concurrently attempt to claim op-a and op-b using two independent connections
    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let store1_task = store1.clone();
    let barrier1 = barrier.clone();
    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;
        store1_task.claim_for_execution("op-concurrent-a").await
    });

    let store2_task = store2.clone();
    let barrier2 = barrier.clone();
    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;
        store2_task.claim_for_execution("op-concurrent-b").await
    });

    let res_a = handle1.await.unwrap().unwrap();
    let res_b = handle2.await.unwrap().unwrap();

    // Operation A (sequence 1) must be claimed
    assert!(res_a, "op-concurrent-a must be successfully claimed");
    // Operation B (sequence 2) MUST NOT be claimed
    assert!(!res_b, "op-concurrent-b must NOT be claimed concurrently");

    // Check database state
    let a_post = store1
        .get_profile_operation("op-concurrent-a")
        .await
        .unwrap()
        .unwrap();
    let b_post = store1
        .get_profile_operation("op-concurrent-b")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(a_post.status, ProfileOperationStatus::Dispatching);
    assert_eq!(b_post.status, ProfileOperationStatus::Pending);

    // Crucial check: it is impossible for both to be Dispatching
    assert!(
        !(a_post.status == ProfileOperationStatus::Dispatching
            && b_post.status == ProfileOperationStatus::Dispatching),
        "concurrent claims must never produce two Dispatching operations for the same field"
    );

    // Complete op-a
    store1
        .record_completed("op-concurrent-a", None)
        .await
        .unwrap();

    // Now op-b can be claimed via store2
    assert!(store2.claim_for_execution("op-concurrent-b").await.unwrap());
    let b_final = store2
        .get_profile_operation("op-concurrent-b")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b_final.status, ProfileOperationStatus::Dispatching);
}

#[tokio::test]
async fn concurrent_claims_on_same_operation_yields_exactly_one_winner() {
    let db_url = test_db_url("concurrent_same_op");
    let store1 = Arc::new(DbStore::connect(&db_url).await.expect("connect store 1"));
    let store2 = Arc::new(DbStore::connect(&db_url).await.expect("connect store 2"));

    let site = SiteId::from("blog");
    let author = "pubkey-same-op";
    let target = ProfileTargetValue::SetDisplayName("Solo".to_string());

    store1
        .claim_or_get_profile_operation("op-solo", author, &site, &target)
        .await
        .unwrap();

    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let store1_task = store1.clone();
    let barrier1 = barrier.clone();
    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;
        store1_task.claim_for_execution("op-solo").await
    });

    let store2_task = store2.clone();
    let barrier2 = barrier.clone();
    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;
        store2_task.claim_for_execution("op-solo").await
    });

    let res1 = handle1.await.unwrap().unwrap();
    let res2 = handle2.await.unwrap().unwrap();

    // Exactly one winner
    assert!(
        res1 ^ res2,
        "exactly one worker must succeed in claiming op-solo, got ({res1}, {res2})"
    );

    let op = store1
        .get_profile_operation("op-solo")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op.status, ProfileOperationStatus::Dispatching);
}

#[tokio::test]
async fn concurrent_claims_on_different_fields_both_succeed() {
    let db_url = test_db_url("concurrent_different_fields");
    let store1 = Arc::new(DbStore::connect(&db_url).await.expect("connect store 1"));
    let store2 = Arc::new(DbStore::connect(&db_url).await.expect("connect store 2"));

    let site = SiteId::from("blog");
    let author = "pubkey-multi-concurrent";

    let op_name = ProfileTargetValue::SetDisplayName("Name".to_string());
    let media = MediaReference::new_v4();
    let op_avatar = ProfileTargetValue::SetAvatar(media);

    store1
        .claim_or_get_profile_operation("op-diff-name", author, &site, &op_name)
        .await
        .unwrap();
    store1
        .claim_or_get_profile_operation("op-diff-avatar", author, &site, &op_avatar)
        .await
        .unwrap();

    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let store1_task = store1.clone();
    let barrier1 = barrier.clone();
    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;
        store1_task.claim_for_execution("op-diff-name").await
    });

    let store2_task = store2.clone();
    let barrier2 = barrier.clone();
    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;
        store2_task.claim_for_execution("op-diff-avatar").await
    });

    let res1 = handle1.await.unwrap().unwrap();
    let res2 = handle2.await.unwrap().unwrap();

    // Both succeed concurrently because DisplayName and Avatar are independent fields!
    assert!(res1, "DisplayName claim must succeed");
    assert!(res2, "Avatar claim must succeed");

    let op_n = store1
        .get_profile_operation("op-diff-name")
        .await
        .unwrap()
        .unwrap();
    let op_a = store1
        .get_profile_operation("op-diff-avatar")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(op_n.status, ProfileOperationStatus::Dispatching);
    assert_eq!(op_a.status, ProfileOperationStatus::Dispatching);
}
