use chrono::Utc;
use cumments_core::models::{PostSlug, RoomMember, RoomStateEvent, SiteId};
use cumments_core::ports::{RegistryStore, RoomStore};
use cumments_store::DbStore;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-room-test-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

#[tokio::test]
async fn members_upsert_and_lookup() {
    let store = DbStore::connect(&test_db_url("members"))
        .await
        .expect("connect db");
    store
        .save_member(&RoomMember {
            room_id: "!room:hs".to_string(),
            user_id: "@alice:hs".to_string(),
            display_name: Some("Alice".to_string()),
            avatar_url: None,
            membership: "join".to_string(),
            updated_at: Utc::now(),
        })
        .await
        .expect("save member");

    let member = store
        .get_member("!room:hs", "@alice:hs")
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.display_name.as_deref(), Some("Alice"));
    assert_eq!(member.membership, "join");

    // Upsert updates the profile.
    store
        .save_member(&RoomMember {
            room_id: "!room:hs".to_string(),
            user_id: "@alice:hs".to_string(),
            display_name: Some("Alice B".to_string()),
            avatar_url: Some("mxc://hs/a".to_string()),
            membership: "leave".to_string(),
            updated_at: Utc::now(),
        })
        .await
        .expect("update member");
    let member = store
        .get_member("!room:hs", "@alice:hs")
        .await
        .expect("get member")
        .expect("member exists");
    assert_eq!(member.display_name.as_deref(), Some("Alice B"));
    assert_eq!(member.membership, "leave");
}

#[tokio::test]
async fn metadata_uses_latest_state_events_and_counts_joined() {
    let store = DbStore::connect(&test_db_url("metadata"))
        .await
        .expect("connect db");

    for (user, membership) in [
        ("@alice:hs", "join"),
        ("@bob:hs", "join"),
        ("@carol:hs", "leave"),
    ] {
        store
            .save_member(&RoomMember {
                room_id: "!room:hs".to_string(),
                user_id: user.to_string(),
                display_name: None,
                avatar_url: None,
                membership: membership.to_string(),
                updated_at: Utc::now(),
            })
            .await
            .expect("save member");
    }

    store
        .save_state_event(&RoomStateEvent {
            event_id: "$name1:hs".to_string(),
            room_id: "!room:hs".to_string(),
            event_type: "m.room.name".to_string(),
            state_key: String::new(),
            sender: "@alice:hs".to_string(),
            origin_server_ts: 100,
            content_json: serde_json::json!({ "name": "old" }),
        })
        .await
        .expect("save old name");
    store
        .save_state_event(&RoomStateEvent {
            event_id: "$name2:hs".to_string(),
            room_id: "!room:hs".to_string(),
            event_type: "m.room.name".to_string(),
            state_key: String::new(),
            sender: "@alice:hs".to_string(),
            origin_server_ts: 200,
            content_json: serde_json::json!({ "name": "new" }),
        })
        .await
        .expect("save new name");
    store
        .save_state_event(&RoomStateEvent {
            event_id: "$topic:hs".to_string(),
            room_id: "!room:hs".to_string(),
            event_type: "m.room.topic".to_string(),
            state_key: String::new(),
            sender: "@alice:hs".to_string(),
            origin_server_ts: 150,
            content_json: serde_json::json!({ "topic": "hello world" }),
        })
        .await
        .expect("save topic");

    let metadata = store
        .get_room_metadata("!room:hs")
        .await
        .expect("get metadata")
        .expect("metadata exists");
    assert_eq!(metadata.name.as_deref(), Some("new"));
    assert_eq!(metadata.topic.as_deref(), Some("hello world"));
    assert_eq!(metadata.member_count, 2);

    let messages = store
        .get_room_system_messages("!room:hs", 10)
        .await
        .expect("system messages");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].event_id, "$name2:hs");
}

#[tokio::test]
async fn list_active_rooms_returns_room_ids() {
    let store = DbStore::connect(&test_db_url("active-rooms"))
        .await
        .expect("connect db");
    store
        .register_room(
            "!room:hs",
            &SiteId::from("my-blog"),
            &PostSlug::from("hello"),
        )
        .await
        .expect("register room");

    let rooms = store.list_active_rooms().await.expect("list active rooms");
    assert_eq!(rooms, vec!["!room:hs"]);

    let site_rooms = store
        .list_active_rooms_for_site(&SiteId::from("my-blog"))
        .await
        .expect("list active rooms for site");
    assert_eq!(site_rooms, vec!["!room:hs"]);
}

#[tokio::test]
async fn state_events_can_be_read_and_content_replaced_by_event_id() {
    let store = DbStore::connect(&test_db_url("state-event-content"))
        .await
        .expect("connect db");
    store
        .save_state_event(&RoomStateEvent {
            event_id: "$name:hs".to_string(),
            room_id: "!room:hs".to_string(),
            event_type: "m.room.name".to_string(),
            state_key: String::new(),
            sender: "@alice:hs".to_string(),
            origin_server_ts: 100,
            content_json: serde_json::json!({ "name": "old" }),
        })
        .await
        .expect("save state event");

    let stored = store
        .get_state_event("$name:hs")
        .await
        .expect("get state event")
        .expect("state event exists");
    assert_eq!(stored.event_type, "m.room.name");
    assert_eq!(stored.content_json["name"], "old");

    // Redaction stripping replaces the content in place.
    assert!(
        store
            .update_state_event_content("$name:hs", &serde_json::json!({}))
            .await
            .expect("update content")
    );
    let stripped = store
        .get_state_event("$name:hs")
        .await
        .expect("get state event")
        .expect("state event exists");
    assert_eq!(stripped.content_json, serde_json::json!({}));
    assert_eq!(stripped.sender, "@alice:hs", "other columns stay intact");

    // Unknown events are a clean no-op.
    assert!(
        !store
            .update_state_event_content("$missing:hs", &serde_json::json!({}))
            .await
            .expect("update missing")
    );
    assert!(
        store
            .get_state_event("$missing:hs")
            .await
            .expect("get missing")
            .is_none()
    );
}

#[tokio::test]
async fn latest_state_event_orders_by_timestamp_then_event_id() {
    let store = DbStore::connect(&test_db_url("latest-state-event"))
        .await
        .expect("connect db");
    for (event_id, ts, name) in [
        ("$v1:hs", 100, "old"),
        ("$v2:hs", 200, "newer"),
        ("$v3:hs", 200, "tie"), // same ts, later event id wins
    ] {
        store
            .save_state_event(&RoomStateEvent {
                event_id: event_id.to_string(),
                room_id: "!room:hs".to_string(),
                event_type: "m.room.name".to_string(),
                state_key: String::new(),
                sender: "@alice:hs".to_string(),
                origin_server_ts: ts,
                content_json: serde_json::json!({ "name": name }),
            })
            .await
            .expect("save state event");
    }

    let latest = store
        .get_latest_state_event("!room:hs", "m.room.name", "")
        .await
        .expect("get latest")
        .expect("latest exists");
    assert_eq!(latest.event_id, "$v3:hs");
    assert_eq!(latest.content_json["name"], "tie");
}
