use cumments_core::{models::SiteId, ports::VirtualUserStore};
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-test-identity-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).expect("create db file");
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn virtual_users_are_listed_per_site() {
    let store = DbStore::connect(&test_db_url("virtual-users"))
        .await
        .expect("connect db");
    let site_a = SiteId::new("site-a".to_string()).expect("site id");
    let site_b = SiteId::new("site-b".to_string()).expect("site id");

    let u1 = store
        .get_or_create_virtual_user("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc", &site_a, "hs")
        .await
        .expect("create u1");
    let u2 = store
        .get_or_create_virtual_user(&"A".repeat(43), &site_a, "hs")
        .await
        .expect("create u2");
    let _u3 = store
        .get_or_create_virtual_user(&("AQEB".repeat(10) + "AQE"), &site_b, "hs")
        .await
        .expect("create u3");

    let mut users = store
        .list_virtual_users_for_site(&site_a)
        .await
        .expect("list site users");
    let mut expected = vec![u1, u2];
    users.sort();
    expected.sort();
    assert_eq!(users, expected);
    assert!(
        store
            .list_virtual_users_for_site(&site_b)
            .await
            .unwrap()
            .len()
            == 1
    );
}
