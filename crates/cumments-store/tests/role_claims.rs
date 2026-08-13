use chrono::{Duration, Utc};
use cumments_core::governance::{NewRoleClaim, RoleClaimStatus};
use cumments_core::ports::RoleClaimStore;
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-claims-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

fn new_claim(user_id: &str, level: i64, expires_at: chrono::DateTime<Utc>) -> NewRoleClaim {
    NewRoleClaim {
        site_id: "my-blog".to_string(),
        room_id: String::new(),
        user_id: user_id.to_string(),
        level,
        token_hash: format!("hash-of-{user_id}-{level}"),
        expires_at,
    }
}

#[tokio::test]
async fn claim_lifecycle_pending_activated_applied() {
    let store = DbStore::connect(&test_db_url("lifecycle"))
        .await
        .expect("connect db");

    let claim = store
        .upsert_role_claim(&new_claim(
            "@alice:hs",
            100,
            Utc::now() + Duration::hours(24),
        ))
        .await
        .expect("create claim");
    assert_eq!(claim.status, RoleClaimStatus::Pending);
    assert_eq!(
        store
            .pending_claims_for_user("@alice:hs")
            .await
            .expect("pending list")
            .len(),
        1
    );

    assert!(
        store
            .mark_claim_activated(claim.id)
            .await
            .expect("activate"),
        "pending claim should activate"
    );
    assert_eq!(
        store
            .activated_unapplied_claims()
            .await
            .expect("activated list")
            .len(),
        1
    );

    store.mark_claim_applied(claim.id).await.expect("apply");
    assert!(
        store
            .activated_unapplied_claims()
            .await
            .expect("activated list")
            .is_empty()
    );
}

#[tokio::test]
async fn reissuing_a_role_rotates_the_token() {
    let store = DbStore::connect(&test_db_url("rotate"))
        .await
        .expect("connect db");
    let expires = Utc::now() + Duration::hours(24);

    let first = store
        .upsert_role_claim(&new_claim("@alice:hs", 75, expires))
        .await
        .expect("first issue");
    let mut rotated = new_claim("@alice:hs", 75, expires);
    rotated.token_hash = "rotated-token".to_string();
    let second = store
        .upsert_role_claim(&rotated)
        .await
        .expect("rotate token");

    assert_eq!(first.id, second.id, "same scope must reuse the claim row");
    assert_eq!(second.token_hash, "rotated-token");
    assert_eq!(
        store
            .pending_claims_for_user("@alice:hs")
            .await
            .expect("pending list")
            .len(),
        1
    );
}

#[tokio::test]
async fn revoke_cancels_unapplied_claims_and_expired_rows_are_purged() {
    let store = DbStore::connect(&test_db_url("revoke"))
        .await
        .expect("connect db");

    let claim = store
        .upsert_role_claim(&new_claim(
            "@alice:hs",
            50,
            Utc::now() + Duration::hours(24),
        ))
        .await
        .expect("create claim");
    assert_eq!(claim.status, RoleClaimStatus::Pending);
    assert!(
        store
            .revoke_role_claim("my-blog", "", "@alice:hs", 50)
            .await
            .expect("revoke"),
        "first revoke cancels the pending claim"
    );
    assert!(
        !store
            .revoke_role_claim("my-blog", "", "@alice:hs", 50)
            .await
            .expect("revoke again"),
        "already revoked claim is not cancellable again"
    );
    assert!(
        store
            .pending_claims_for_user("@alice:hs")
            .await
            .expect("pending list")
            .is_empty()
    );

    // An expired pending claim disappears from queries and from the purge.
    store
        .upsert_role_claim(&new_claim("@bob:hs", 50, Utc::now() - Duration::seconds(1)))
        .await
        .expect("expired claim");
    assert_eq!(store.purge_expired_claims().await.expect("purge"), 1);
    assert!(
        store
            .pending_claims_for_user("@bob:hs")
            .await
            .expect("pending list")
            .is_empty()
    );
}
