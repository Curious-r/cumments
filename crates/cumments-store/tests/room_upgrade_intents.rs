use cumments_core::{models::RoomUpgradeIntentStatus, ports::RegistryStore};
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-test-upgrade-intent-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).expect("create db file");
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn upgrade_intent_tracks_observation_and_adoption() {
    let store = DbStore::connect(&test_db_url("lifecycle"))
        .await
        .expect("connect db");

    store
        .record_upgrade_intent("!old:hs", "12")
        .await
        .expect("record intent");
    let observed = store
        .observe_upgrade_replacement("!old:hs", "!new:hs")
        .await
        .expect("observe replacement")
        .expect("open intent");
    assert_eq!(observed.status, RoomUpgradeIntentStatus::Observed);
    assert_eq!(observed.replacement_room_id.as_deref(), Some("!new:hs"));

    let completed = store
        .complete_upgrade_intent("!old:hs", "!new:hs")
        .await
        .expect("complete intent")
        .expect("matching intent");
    assert_eq!(completed.status, RoomUpgradeIntentStatus::Adopted);

    // A replay of the same tombstone after registry convergence is a no-op.
    let replay = store
        .complete_upgrade_intent("!old:hs", "!new:hs")
        .await
        .expect("replay completion")
        .expect("matching intent");
    assert_eq!(replay.status, RoomUpgradeIntentStatus::Adopted);
}

#[tokio::test]
async fn manual_intent_is_terminal_until_explicitly_reset_elsewhere() {
    let store = DbStore::connect(&test_db_url("manual"))
        .await
        .expect("connect db");

    store
        .record_upgrade_intent("!old:hs", "12")
        .await
        .expect("record intent");
    store
        .mark_upgrade_intent_manual("!old:hs", "unexpected successor")
        .await
        .expect("mark manual");
    let intent = store
        .observe_upgrade_replacement("!old:hs", "!new:hs")
        .await
        .expect("observe terminal intent")
        .expect("intent");

    assert_eq!(intent.status, RoomUpgradeIntentStatus::Manual);
    assert_eq!(
        intent.error_message.as_deref(),
        Some("unexpected successor")
    );
}

#[tokio::test]
async fn operator_approval_recovers_an_exact_failed_intent() {
    let store = DbStore::connect(&test_db_url("approve-failed"))
        .await
        .expect("connect db");

    store
        .record_upgrade_intent("!old:hs", "13")
        .await
        .expect("record intent");
    store
        .observe_upgrade_replacement("!old:hs", "!new:hs")
        .await
        .expect("observe replacement");
    store
        .fail_upgrade_intent("!old:hs", "convergence failed")
        .await
        .expect("fail intent");

    let recovered = store
        .approve_upgrade_intent_recovery("!old:hs", "13", "!new:hs")
        .await
        .expect("approve recovery");
    assert_eq!(recovered.status, RoomUpgradeIntentStatus::Requested);
    assert_eq!(recovered.replacement_room_id.as_deref(), Some("!new:hs"));
    assert_eq!(recovered.error_message, None);

    let intents = store.list_upgrade_intents().await.expect("list intents");
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].status, RoomUpgradeIntentStatus::Requested);
}

#[tokio::test]
async fn operator_approval_rejects_details_that_do_not_match_the_intent() {
    let store = DbStore::connect(&test_db_url("approve-mismatch"))
        .await
        .expect("connect db");

    store
        .record_upgrade_intent("!old:hs", "13")
        .await
        .expect("record intent");
    store
        .observe_upgrade_replacement("!old:hs", "!new:hs")
        .await
        .expect("observe replacement");
    store
        .mark_upgrade_intent_manual("!old:hs", "manual review")
        .await
        .expect("mark manual");

    assert!(
        store
            .approve_upgrade_intent_recovery("!old:hs", "12", "!new:hs")
            .await
            .is_err(),
        "version must match the durable authorization"
    );
    assert!(
        store
            .approve_upgrade_intent_recovery("!old:hs", "13", "!other:hs")
            .await
            .is_err(),
        "replacement must match the durable authorization"
    );

    let intent = store.get_upgrade_intent("!old:hs").await.unwrap().unwrap();
    assert_eq!(intent.status, RoomUpgradeIntentStatus::Manual);
}
