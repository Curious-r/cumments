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
