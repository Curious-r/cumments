//! Matrix redaction semantics (room versions 11+).
//!
//! Redacting an event strips its content down to the keys protected by the
//! room-version redaction algorithm. The projector applies this in place so
//! live pushes and backfill replay produce the same read model.

use serde_json::{Map, Value};

/// Strips `content` according to the v11/v12 redaction algorithm.
///
/// Most event types end up with an empty object (the state slot survives but
/// its content is removed). `m.room.member`, `m.room.create`,
/// `m.room.join_rules`, `m.room.power_levels`, `m.room.history_visibility`
/// and `m.room.redaction` keep their protected keys.
pub fn redact_state_content(event_type: &str, content: &Value) -> Value {
    let object = content.as_object();
    let mut stripped = Map::new();

    let mut keep = |keys: &[&str]| {
        if let Some(object) = object {
            for key in keys {
                if let Some(value) = object.get(*key) {
                    stripped.insert((*key).to_string(), value.clone());
                }
            }
        }
    };

    match event_type {
        "m.room.member" => {
            keep(&["membership", "join_authorised_via_users_server"]);
            if let Some(invite) = object.and_then(|o| o.get("third_party_invite"))
                && let Some(signed) = invite.get("signed")
            {
                let mut invite_stripped = Map::new();
                invite_stripped.insert("signed".to_string(), signed.clone());
                stripped.insert(
                    "third_party_invite".to_string(),
                    Value::Object(invite_stripped),
                );
            }
        }
        // `m.room.create` keeps its entire content.
        "m.room.create" => return content.clone(),
        "m.room.join_rules" => keep(&["join_rule", "allow"]),
        "m.room.power_levels" => keep(&[
            "ban",
            "events",
            "events_default",
            "invite",
            "kick",
            "redact",
            "state_default",
            "users",
            "users_default",
        ]),
        "m.room.history_visibility" => keep(&["history_visibility"]),
        "m.room.redaction" => keep(&["redacts"]),
        _ => {}
    }

    Value::Object(stripped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metadata_events_are_emptied() {
        assert_eq!(
            redact_state_content("m.room.name", &json!({ "name": "old" })),
            json!({})
        );
        assert_eq!(
            redact_state_content("m.room.topic", &json!({ "topic": "old" })),
            json!({})
        );
        assert_eq!(
            redact_state_content("m.room.avatar", &json!({ "url": "mxc://hs/a" })),
            json!({})
        );
    }

    #[test]
    fn member_keeps_membership_and_drops_profile() {
        assert_eq!(
            redact_state_content(
                "m.room.member",
                &json!({
                    "membership": "join",
                    "displayname": "Alice",
                    "avatar_url": "mxc://hs/a",
                    "join_authorised_via_users_server": "@mod:hs",
                }),
            ),
            json!({
                "membership": "join",
                "join_authorised_via_users_server": "@mod:hs",
            })
        );
    }

    #[test]
    fn power_levels_keeps_governance_keys() {
        let stripped = redact_state_content(
            "m.room.power_levels",
            &json!({
                "users": { "@owner:hs": 100 },
                "events": { "m.room.power_levels": 100 },
                "state_default": 50,
                "notifications": { "room": 50 },
                "invite": 0,
            }),
        );
        assert_eq!(stripped["users"]["@owner:hs"], 100);
        assert_eq!(stripped["events"]["m.room.power_levels"], 100);
        assert_eq!(stripped["state_default"], 50);
        assert_eq!(stripped["invite"], 0);
        assert!(stripped.get("notifications").is_none());
    }

    #[test]
    fn create_keeps_everything_and_join_rules_history_keep_their_fields() {
        let create = json!({ "room_version": "12", "type": "m.space" });
        assert_eq!(redact_state_content("m.room.create", &create), create);
        assert_eq!(
            redact_state_content(
                "m.room.join_rules",
                &json!({ "join_rule": "public", "allow": [], "extra": 1 }),
            ),
            json!({ "join_rule": "public", "allow": [] })
        );
        assert_eq!(
            redact_state_content(
                "m.room.history_visibility",
                &json!({ "history_visibility": "shared", "extra": 1 }),
            ),
            json!({ "history_visibility": "shared" })
        );
    }

    #[test]
    fn unknown_types_are_emptied() {
        assert_eq!(
            redact_state_content("m.room.pinned_events", &json!(["$a", "$b"])),
            json!({})
        );
    }
}
