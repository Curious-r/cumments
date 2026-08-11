use cumments_core::ports::SiteStore;
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
async fn ensure_site_exists_is_idempotent_for_existing_sites() {
    let store = DbStore::connect(&test_db_url("site-exists"))
        .await
        .expect("connect db");

    // First call creates the site.
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("create site");

    // Calling it again with the same site must succeed instead of failing
    // with "None of the records are inserted" (ON CONFLICT DO NOTHING).
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("existing site is a no-op");

    let site = store
        .get_site(&cumments_core::models::SiteId::from("my-blog"))
        .await
        .expect("query site")
        .expect("site exists");
    assert_eq!(site.matrix_space_id, "!space:hs");
}
