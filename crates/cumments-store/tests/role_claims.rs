use chrono::{Duration, Utc};
use cumments_core::{
    governance::{NewRoleClaim, OWNER_LEVEL, RoleClaimStatus},
    ports::RoleClaimStore,
};
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-test-role-claims-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).expect("create db file");
    format!("sqlite://{}", path.display())
}

fn new_claim(user_id: &str, expires_in: chrono::Duration) -> NewRoleClaim {
    NewRoleClaim {
        site_id: "my-blog".to_string(),
        room_id: String::new(),
        user_id: user_id.to_string(),
        level: OWNER_LEVEL,
        token_hash: "token-hash".to_string(),
        expires_at: Utc::now() + expires_in,
    }
}

#[tokio::test]
async fn claim_dm_room_is_tracked_and_cleared_on_regrant() {
    let store = DbStore::connect(&test_db_url("dm-room"))
        .await
        .expect("connect db");
    store
        .upsert_role_claim(&new_claim("@u:hs", Duration::hours(1)))
        .await
        .expect("upsert");

    store
        .set_claim_dm_room_for_user("@u:hs", "!dm:hs")
        .await
        .expect("set dm room");
    assert!(store.claim_dm_room_exists("!dm:hs").await.unwrap());
    assert_eq!(
        store.claim_dm_rooms().await.unwrap(),
        vec![("@u:hs".to_string(), "!dm:hs".to_string())]
    );
    assert!(
        store
            .active_claims_in_dm_room("@u:hs", "!dm:hs")
            .await
            .unwrap()
    );

    // Re-granting rotates the claim back to pending without inheriting the
    // old DM room, so the bot treats it as a fresh verification flow.
    store
        .upsert_role_claim(&new_claim("@u:hs", Duration::hours(1)))
        .await
        .expect("re-grant");
    assert!(!store.claim_dm_room_exists("!dm:hs").await.unwrap());
    assert!(store.claim_dm_rooms().await.unwrap().is_empty());
    assert!(
        !store
            .active_claims_in_dm_room("@u:hs", "!dm:hs")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn applied_claim_is_revoked_after_matrix_role_removal() {
    let store = DbStore::connect(&test_db_url("applied-revoke"))
        .await
        .expect("connect db");
    store
        .upsert_role_claim(&new_claim("@u:hs", Duration::hours(1)))
        .await
        .expect("upsert");

    let claim = store
        .pending_claims_for_user("@u:hs")
        .await
        .expect("pending")
        .remove(0);
    assert!(store.mark_claim_activated(claim.id).await.unwrap());
    store
        .mark_claim_applied(store.activated_unapplied_claims().await.unwrap()[0].id)
        .await
        .expect("mark applied");

    let applied = store.list_applied_claims().await.unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].status, RoleClaimStatus::Applied);

    // An applied claim is not cancellable through the pending path...
    assert!(
        !store
            .revoke_role_claim("my-blog", "", "@u:hs", OWNER_LEVEL)
            .await
            .unwrap()
    );
    // ...but the applied-revocation path (used after the Matrix write) works.
    assert!(
        store
            .mark_applied_claim_revoked("my-blog", "", "@u:hs", OWNER_LEVEL)
            .await
            .unwrap()
    );
    assert!(store.list_applied_claims().await.unwrap().is_empty());
}

#[tokio::test]
async fn expired_claims_do_not_keep_the_bot_in_a_dm() {
    let store = DbStore::connect(&test_db_url("expired-dm"))
        .await
        .expect("connect db");
    store
        .upsert_role_claim(&new_claim("@u:hs", Duration::seconds(-1)))
        .await
        .expect("upsert");
    store
        .set_claim_dm_room_for_user("@u:hs", "!dm:hs")
        .await
        .expect("set dm room");

    assert!(
        !store
            .active_claims_in_dm_room("@u:hs", "!dm:hs")
            .await
            .unwrap()
    );
    assert_eq!(store.purge_expired_claims().await.unwrap(), 1);
    assert!(!store.claim_dm_room_exists("!dm:hs").await.unwrap());
}

#[tokio::test]
async fn claim_dm_rooms_are_listed_per_site_for_retirement() {
    let store = DbStore::connect(&test_db_url("dm-by-site"))
        .await
        .expect("connect db");
    for (site, user, room) in [
        ("site-a", "@u1:hs", "!dm-a:hs"),
        ("site-a", "@u2:hs", "!dm-b:hs"),
        ("site-b", "@u3:hs", "!dm-c:hs"),
    ] {
        let mut claim = new_claim(user, Duration::hours(1));
        claim.site_id = site.to_string();
        store.upsert_role_claim(&claim).await.expect("upsert");
        store
            .set_claim_dm_room_for_user(user, room)
            .await
            .expect("set dm room");
    }

    let mut pairs = store
        .claim_dm_rooms_for_site("site-a")
        .await
        .expect("claim dms for site");
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("@u1:hs".to_string(), "!dm-a:hs".to_string()),
            ("@u2:hs".to_string(), "!dm-b:hs".to_string()),
        ]
    );
}
