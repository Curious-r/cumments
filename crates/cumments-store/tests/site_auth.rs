use cumments_core::ports::SiteAuthStore;
use cumments_core::site_auth::{
    NewVerificationToken, Origin, SiteAuthMode, SiteVerificationStatus, VerificationMethod,
    token_hash,
};
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-test-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn site_lifecycle_register_verify_secret() {
    let store = DbStore::connect(&test_db_url("site-auth"))
        .await
        .expect("connect db");
    let site_id = "a1b2c3d4e5f60718a1b2c3d4e5f60718";
    let claim_token = "claim-token-value";

    store
        .register_site(site_id, &token_hash(claim_token))
        .await
        .expect("register site");

    let auth = store
        .get_site_auth(site_id)
        .await
        .expect("query auth")
        .expect("site exists");
    assert_eq!(auth.auth_mode, SiteAuthMode::Origin);
    assert_eq!(auth.verification_status, SiteVerificationStatus::Unverified);
    assert!(auth.verified_origins.is_empty());
    assert_eq!(
        store
            .get_claim_token_hash(site_id)
            .await
            .expect("claim hash")
            .as_deref(),
        Some(token_hash(claim_token).as_str())
    );

    let origin = Origin::parse("https://blog.example.com").expect("valid origin");
    store
        .insert_verification_tokens(&[NewVerificationToken {
            site_id: site_id.to_string(),
            origin: origin.clone(),
            token_hash: token_hash("proof-token"),
            methods: vec![VerificationMethod::WellKnown],
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        }])
        .await
        .expect("insert tokens");

    let token = store
        .find_verification_token(site_id, &origin, &token_hash("proof-token"))
        .await
        .expect("find token")
        .expect("token exists");
    assert_eq!(token.methods, vec![VerificationMethod::WellKnown]);

    assert!(
        store
            .consume_verification_token(token.id)
            .await
            .expect("consume once")
    );
    assert!(
        !store
            .consume_verification_token(token.id)
            .await
            .expect("consume twice is a no-op")
    );

    store
        .add_verified_origin(site_id, &origin)
        .await
        .expect("add origin");
    let auth = store
        .get_site_auth(site_id)
        .await
        .expect("query auth")
        .expect("site exists");
    assert_eq!(auth.verification_status, SiteVerificationStatus::Verified);
    assert_eq!(auth.verified_origins, vec![origin]);

    store
        .store_site_secret(site_id, "super-secret-hmac-key")
        .await
        .expect("store secret");
    let auth = store
        .get_site_auth(site_id)
        .await
        .expect("query auth")
        .expect("site exists");
    assert_eq!(auth.auth_mode, SiteAuthMode::Secret);
    assert_eq!(auth.secret.as_deref(), Some("super-secret-hmac-key"));
}

#[tokio::test]
async fn admin_operations_list_revoke_and_clear_secret() {
    let store = DbStore::connect(&test_db_url("site-auth-admin"))
        .await
        .expect("connect db");
    let site_id = "c3d4e5f60718a1b2c3d4e5f60718a1b2";
    store
        .register_site(site_id, &token_hash("claim"))
        .await
        .expect("register site");

    let origin = Origin::parse("https://blog.example.com").expect("valid origin");
    store
        .add_verified_origin(site_id, &origin)
        .await
        .expect("add origin");

    let listed = store.list_site_auth().await.expect("list sites");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].site_id, site_id);
    assert_eq!(listed[0].verified_origins, vec![origin.clone()]);
    assert!(listed[0].claim_token_hash.is_some());

    assert!(
        !store
            .revoke_verified_origin(site_id, &Origin::parse("https://nope.example.com").unwrap())
            .await
            .expect("missing origin is a no-op")
    );
    assert!(
        store
            .revoke_verified_origin(site_id, &origin)
            .await
            .expect("revoke origin")
    );
    let auth = store
        .get_site_auth(site_id)
        .await
        .expect("query auth")
        .expect("site exists");
    assert_eq!(auth.verification_status, SiteVerificationStatus::Unverified);
    assert!(auth.verified_origins.is_empty());

    assert!(
        store
            .clear_site_secret(site_id)
            .await
            .expect("clear secret on site without one")
    );
    store
        .store_site_secret(site_id, "some-hmac-key")
        .await
        .expect("store secret");
    assert!(
        store
            .clear_site_secret(site_id)
            .await
            .expect("clear secret")
    );
    let auth = store
        .get_site_auth(site_id)
        .await
        .expect("query auth")
        .expect("site exists");
    assert_eq!(auth.auth_mode, SiteAuthMode::Origin);
    assert_eq!(auth.secret, None);
}

#[tokio::test]
async fn expired_verification_tokens_are_not_found() {
    let store = DbStore::connect(&test_db_url("site-auth-expiry"))
        .await
        .expect("connect db");
    let site_id = "b2c3d4e5f60718a1b2c3d4e5f60718a1";
    store
        .register_site(site_id, &token_hash("claim"))
        .await
        .expect("register site");

    let origin = Origin::parse("https://example.com").expect("valid origin");
    store
        .insert_verification_tokens(&[NewVerificationToken {
            site_id: site_id.to_string(),
            origin: origin.clone(),
            token_hash: token_hash("stale"),
            methods: vec![VerificationMethod::Dns],
            expires_at: chrono::Utc::now() - chrono::Duration::seconds(1),
        }])
        .await
        .expect("insert token");

    assert!(
        store
            .find_verification_token(site_id, &origin, &token_hash("stale"))
            .await
            .expect("query token")
            .is_none()
    );
}

#[tokio::test]
async fn one_challenge_with_multiple_origins_inserts_distinct_tokens() {
    let store = DbStore::connect(&test_db_url("site-auth-multi-origin"))
        .await
        .expect("connect db");
    let site_id = "d4e5f60718a1b2c3d4e5f60718a1b2c3";
    store
        .register_site(site_id, &token_hash("claim"))
        .await
        .expect("register site");

    let token_hash = token_hash("shared-proof-token");
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(1);
    store
        .insert_verification_tokens(&[
            NewVerificationToken {
                site_id: site_id.to_string(),
                origin: Origin::parse("https://a.example.com").expect("origin"),
                token_hash: token_hash.clone(),
                methods: vec![VerificationMethod::WellKnown],
                expires_at,
            },
            NewVerificationToken {
                site_id: site_id.to_string(),
                origin: Origin::parse("https://b.example.com").expect("origin"),
                token_hash: token_hash.clone(),
                methods: vec![VerificationMethod::WellKnown],
                expires_at,
            },
        ])
        .await
        .expect("insert multiple tokens for one challenge");

    assert!(
        store
            .find_verification_token(
                site_id,
                &Origin::parse("https://a.example.com").expect("origin"),
                &token_hash,
            )
            .await
            .expect("query token")
            .is_some()
    );
    assert!(
        store
            .find_verification_token(
                site_id,
                &Origin::parse("https://b.example.com").expect("origin"),
                &token_hash,
            )
            .await
            .expect("query token")
            .is_some()
    );
}

#[tokio::test]
async fn complete_verification_is_atomic_and_idempotent() {
    let store = DbStore::connect(&test_db_url("site-auth-complete"))
        .await
        .expect("connect db");
    let site_id = "e5f60718a1b2c3d4e5f60718a1b2c3d4";
    store
        .register_site(site_id, &token_hash("claim"))
        .await
        .expect("register site");

    let origin = Origin::parse("https://blog.example.com").expect("valid origin");
    store
        .insert_verification_tokens(&[NewVerificationToken {
            site_id: site_id.to_string(),
            origin: origin.clone(),
            token_hash: token_hash("proof-token"),
            methods: vec![VerificationMethod::WellKnown],
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        }])
        .await
        .expect("insert token");
    let token = store
        .find_verification_token(site_id, &origin, &token_hash("proof-token"))
        .await
        .expect("find token")
        .expect("token exists");

    assert!(
        store
            .complete_verification(site_id, &origin, token.id)
            .await
            .expect("first completion wins")
    );
    assert!(
        !store
            .complete_verification(site_id, &origin, token.id)
            .await
            .expect("second completion is a no-op")
    );

    let auth = store
        .get_site_auth(site_id)
        .await
        .expect("query auth")
        .expect("site exists");
    assert_eq!(auth.verification_status, SiteVerificationStatus::Verified);
    assert_eq!(auth.verified_origins, vec![origin]);
}
