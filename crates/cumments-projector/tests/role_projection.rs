use cumments_core::governance::RoleEntry;
use cumments_core::models::{PostSlug, SiteId};
use cumments_core::ports::{GovernanceStore, RegistryStore, SiteStore};
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::parsed::ParsedRoomState;
use cumments_store::DbStore;
use std::sync::Arc;
use tokio::sync::broadcast;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-role-projection-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

fn state_event(room_id: &str, power_levels: serde_json::Value) -> ParsedRoomState {
    ParsedRoomState {
        room_id: room_id.to_string(),
        event_id: format!("$pl:{}", room_id),
        sender: "@_cumments_bot:hs".to_string(),
        event_type: "m.room.power_levels".to_string(),
        state_key: String::new(),
        origin_server_ts: 1,
        content: power_levels,
    }
}

#[tokio::test]
async fn power_levels_project_site_and_room_roles() {
    let store = Arc::new(
        DbStore::connect(&test_db_url("roles"))
            .await
            .expect("connect db"),
    );
    let site_id = SiteId::new("my-blog".to_string()).expect("valid site id");
    let post_slug = PostSlug::new("hello".to_string()).expect("valid post slug");

    // The space and one comment room exist before the state events arrive.
    store
        .ensure_site_exists("my-blog", "!space:hs")
        .await
        .expect("site");
    store
        .register_room("!room:hs", &site_id, &post_slug)
        .await
        .expect("room");

    let (tx, _rx) = broadcast::channel(16);
    let processor = EventProcessor::new(EventProcessorDeps {
        site_store: store.clone(),
        registry_store: store.clone(),
        message_store: store.clone(),
        room_store: store.clone(),
        governance_store: store.clone(),
        intent_store: store.clone(),
        event_bus: tx,
        server_name: Some("hs".to_string()),
    });

    // The Space's roles exclude the AS-managed sender.
    processor
        .process_room_state(state_event(
            "!space:hs",
            serde_json::json!({
                "users": {
                    "@owner:hs": 100,
                    "@co:hs": 75,
                    "@_cumments_bot:hs": 100,
                    "@plain-member:hs": 0,
                }
            }),
        ))
        .await
        .expect("project space roles");
    assert_eq!(
        store.list_site_roles("my-blog").await.expect("site roles"),
        vec![
            RoleEntry {
                user_id: "@co:hs".into(),
                level: 75
            },
            RoleEntry {
                user_id: "@owner:hs".into(),
                level: 100
            },
        ]
    );

    // A comment room projects its own (independent) moderator roster.
    processor
        .process_room_state(state_event(
            "!room:hs",
            serde_json::json!({
                "users": {
                    "@owner:hs": 100,
                    "@mod:hs": 50,
                    "@_cumments_bot:hs": 100,
                }
            }),
        ))
        .await
        .expect("project room roles");
    assert_eq!(
        store.list_room_roles("!room:hs").await.expect("room roles"),
        vec![
            RoleEntry {
                user_id: "@mod:hs".into(),
                level: 50
            },
            RoleEntry {
                user_id: "@owner:hs".into(),
                level: 100
            },
        ]
    );
}
