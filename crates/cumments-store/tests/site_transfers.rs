use chrono::{Duration, Utc};
use cumments_core::{governance::SiteTransferStatus, ports::SiteTransferStore};
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-test-site-transfers-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).expect("create db file");
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn pending_transfer_is_replaced_and_completed() {
    let store = DbStore::connect(&test_db_url("lifecycle"))
        .await
        .expect("connect db");

    let first = store
        .upsert_pending_transfer("blog", "@alice:hs", Utc::now() + Duration::hours(1))
        .await
        .expect("first transfer");
    assert_eq!(first.status, SiteTransferStatus::Pending);
    assert_eq!(
        store
            .find_pending_transfer("blog")
            .await
            .unwrap()
            .unwrap()
            .id,
        first.id
    );

    let second = store
        .upsert_pending_transfer("blog", "@bob:hs", Utc::now() + Duration::hours(1))
        .await
        .expect("second transfer");
    assert_eq!(second.target_mxid, "@bob:hs");
    assert!(
        store
            .find_pending_transfer("blog")
            .await
            .unwrap()
            .unwrap()
            .id
            == second.id
    );

    assert!(
        store
            .complete_transfer("blog", second.id)
            .await
            .expect("complete")
    );
    assert!(store.find_pending_transfer("blog").await.unwrap().is_none());
    assert!(!store.complete_transfer("blog", second.id).await.unwrap());
}

#[tokio::test]
async fn expired_pending_transfers_are_marked() {
    let store = DbStore::connect(&test_db_url("expiry"))
        .await
        .expect("connect db");
    store
        .upsert_pending_transfer("blog", "@alice:hs", Utc::now() - Duration::minutes(1))
        .await
        .expect("expired transfer");
    assert!(store.find_pending_transfer("blog").await.unwrap().is_none());
    assert_eq!(store.expire_pending_transfers().await.expect("expire"), 1);
}
