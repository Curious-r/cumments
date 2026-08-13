use cumments_core::governance::RoleEntry;
use cumments_core::ports::GovernanceStore;
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-governance-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn site_roles_are_replaced_atomically_and_sorted() {
    let store = DbStore::connect(&test_db_url("site-roles"))
        .await
        .expect("connect db");

    store
        .replace_site_roles(
            "my-blog",
            &[
                RoleEntry {
                    user_id: "@zoe:hs".into(),
                    level: 100,
                },
                RoleEntry {
                    user_id: "@amy:hs".into(),
                    level: 75,
                },
            ],
        )
        .await
        .expect("replace");

    let roles = store.list_site_roles("my-blog").await.expect("list");
    assert_eq!(
        roles,
        vec![
            RoleEntry {
                user_id: "@amy:hs".into(),
                level: 75
            },
            RoleEntry {
                user_id: "@zoe:hs".into(),
                level: 100
            },
        ]
    );

    // A later projection replaces the whole roster; stale entries disappear.
    store
        .replace_site_roles(
            "my-blog",
            &[RoleEntry {
                user_id: "@new:hs".into(),
                level: 100,
            }],
        )
        .await
        .expect("replace again");
    assert_eq!(
        store.list_site_roles("my-blog").await.expect("list"),
        vec![RoleEntry {
            user_id: "@new:hs".into(),
            level: 100
        }]
    );
}

#[tokio::test]
async fn room_roles_are_scoped_per_room() {
    let store = DbStore::connect(&test_db_url("room-roles"))
        .await
        .expect("connect db");

    store
        .replace_room_roles(
            "!a:hs",
            &[RoleEntry {
                user_id: "@mod:hs".into(),
                level: 50,
            }],
        )
        .await
        .expect("replace a");
    store
        .replace_room_roles(
            "!b:hs",
            &[RoleEntry {
                user_id: "@other:hs".into(),
                level: 75,
            }],
        )
        .await
        .expect("replace b");

    assert_eq!(
        store.list_room_roles("!a:hs").await.expect("list a"),
        vec![RoleEntry {
            user_id: "@mod:hs".into(),
            level: 50
        }]
    );
    assert_eq!(
        store.list_room_roles("!b:hs").await.expect("list b"),
        vec![RoleEntry {
            user_id: "@other:hs".into(),
            level: 75
        }]
    );
}
