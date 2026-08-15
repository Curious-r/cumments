//! Site governance roles and power-level manipulation.
//!
//! Governance state lives in Matrix `m.room.power_levels` events; the read
//! model only projects them. These pure helpers encode the level ladder and
//! the transformations used by room creation, the moderation sync loop and
//! the governance API.

use crate::ports::MatrixDriver;
use chrono::{DateTime, Utc};
use ruma_common::UserId;
use serde_json::{Map, Value};
use std::str::FromStr;

/// Matrix state event type carrying room power levels.
pub const POWER_LEVELS_EVENT_TYPE: &str = "m.room.power_levels";

/// Matrix state event type carrying the tombstone that marks a room as
/// upgraded. Room version 12 requires its power level to be explicitly
/// higher than `state_default`.
pub const TOMBSTONE_EVENT_TYPE: &str = "m.room.tombstone";

/// The power level required to send `m.room.tombstone`. Cumments uses the
/// room version 12 recommended value 150: only the room creator (the AS bot,
/// who has infinite power in v12) or a user explicitly boosted to 150 can
/// upgrade a room. Site owners stay at 100 and therefore cannot self-upgrade
/// from a Matrix client — doing so would make them the replacement room's
/// creator with immutable infinite power.
pub const TOMBSTONE_LOCK_LEVEL: i64 = 150;

/// Level ladder for site governance roles.
pub const OWNER_LEVEL: i64 = 100;
pub const CO_MANAGER_LEVEL: i64 = 75;
pub const MODERATOR_LEVEL: i64 = 50;

/// The level required to edit `m.room.power_levels` itself. Only the site
/// owner and the room creator (AS sender) reach it.
pub const ROLE_LOCK_LEVEL: i64 = 100;

/// Entries at or above this level are site-managed: they are replicated from
/// the site Space into every comment room, and the moderation sync loop
/// reconciles them. Per-room moderators (50) live below this line and are
/// never touched by site-level reconciliation.
pub const SITE_ROLE_MIN_LEVEL: i64 = CO_MANAGER_LEVEL;

/// One projected governance entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleEntry {
    pub user_id: String,
    pub level: i64,
}

/// Lifecycle of a token-DM role claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleClaimStatus {
    /// The role is awaiting token verification from the target MXID.
    Pending,
    /// The target MXID proved ownership; the role has not been written to
    /// Matrix yet.
    Activated,
    /// The role was written to Matrix power levels.
    Applied,
    /// The claim was cancelled before (or after) activation.
    Revoked,
}

impl RoleClaimStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Activated => "activated",
            Self::Applied => "applied",
            Self::Revoked => "revoked",
        }
    }
}

impl FromStr for RoleClaimStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "activated" => Ok(Self::Activated),
            "applied" => Ok(Self::Applied),
            "revoked" => Ok(Self::Revoked),
            other => Err(format!("unknown role claim status `{other}`")),
        }
    }
}

/// A pending (or completed) token-DM role claim. This is process state, not
/// the source of truth: the authoritative role lives in Matrix power levels
/// once the claim reaches `applied`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleClaim {
    pub id: i64,
    pub site_id: String,
    /// Empty string for site-level roles (owner/co-manager); otherwise the
    /// comment room this moderator claim targets.
    pub room_id: String,
    /// The DM room the AppService bot joined to verify this claim, if any.
    /// Set on conditional auto-join; used to leave the DM once the claim
    /// reaches a terminal state.
    pub dm_room_id: Option<String>,
    pub user_id: String,
    pub level: i64,
    pub token_hash: String,
    pub status: RoleClaimStatus,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub activated_at: Option<DateTime<Utc>>,
    pub applied_at: Option<DateTime<Utc>>,
}

/// Payload for creating or rotating a role claim.
#[derive(Debug, Clone)]
pub struct NewRoleClaim {
    pub site_id: String,
    pub room_id: String,
    pub user_id: String,
    pub level: i64,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
}

/// Why a Matrix user id cannot hold a governance role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernanceUserIdError {
    /// The string is not a fully qualified Matrix user id.
    Invalid,
    /// The id belongs to a Cumments service account (AS sender or a guest
    /// virtual user).
    ServiceAccount,
}

impl std::fmt::Display for GovernanceUserIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid => f.write_str("invalid Matrix user ID"),
            Self::ServiceAccount => {
                f.write_str("Cumments service accounts cannot hold governance roles")
            }
        }
    }
}

impl std::error::Error for GovernanceUserIdError {}

/// Validates a Matrix user id for a governance role: it must parse as a fully
/// qualified MXID and must not be a Cumments service account.
pub fn validate_governance_user_id(raw: &str) -> Result<String, GovernanceUserIdError> {
    let parsed = UserId::parse(raw).map_err(|_| GovernanceUserIdError::Invalid)?;
    let user_id = parsed.as_str().to_string();
    if is_as_managed_user(&user_id) {
        return Err(GovernanceUserIdError::ServiceAccount);
    }
    Ok(user_id)
}

fn users_map(power_levels: &Value) -> Option<&Map<String, Value>> {
    power_levels.get("users")?.as_object()
}

fn users_map_mut(power_levels: &mut Value) -> &mut Map<String, Value> {
    power_levels
        .as_object_mut()
        .expect("power levels are a JSON object")
        .entry("users")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("users is an object")
}

/// Whether a Matrix user ID is managed by the Cumments AppService
/// (the AS sender or a guest virtual user). These accounts are implementation
/// details and must never be projected or treated as governance roles.
pub fn is_as_managed_user(user_id: &str) -> bool {
    let Some(localpart) = user_id
        .strip_prefix('@')
        .and_then(|s| s.split_once(':').map(|(localpart, _)| localpart))
    else {
        return false;
    };
    localpart.starts_with("_cumments_")
}

/// The `users` entries at or above `min_level`, sorted by user ID for
/// deterministic output.
pub fn role_entries(power_levels: &Value, min_level: i64) -> Vec<RoleEntry> {
    let Some(users) = users_map(power_levels) else {
        return Vec::new();
    };
    let mut entries: Vec<RoleEntry> = users
        .iter()
        .filter_map(|(user_id, level)| {
            let level = level.as_i64()?;
            (level >= min_level).then(|| RoleEntry {
                user_id: user_id.clone(),
                level,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.user_id.cmp(&b.user_id));
    entries
}

/// Initial power levels for a site Space: the creator entry (pre-v12 rooms)
/// plus the governance locks. `state_default` is lowered to the moderator
/// level so site co-managers can manage Space structure, but the power-levels
/// event stays owner-only and the tombstone stays bot-only (150). In
/// pre-v12 rooms (where the bot has no implicit creator power) the sender
/// entry is set to the tombstone lock level so the bot can later upgrade the
/// room; v12 rooms omit the entry and rely on implicit creator power.
pub fn initial_space_power_levels(sender_user_id: &str, include_sender: bool) -> Value {
    let mut users = Map::new();
    if include_sender {
        users.insert(
            sender_user_id.to_string(),
            Value::from(TOMBSTONE_LOCK_LEVEL),
        );
    }
    serde_json::json!({
        "users": users,
        "events": {
            POWER_LEVELS_EVENT_TYPE: ROLE_LOCK_LEVEL,
            TOMBSTONE_EVENT_TYPE: TOMBSTONE_LOCK_LEVEL,
        },
        "state_default": MODERATOR_LEVEL,
    })
}

/// Initial power levels for a comment room, seeded from the site Space: every
/// site-managed entry (owner and co-managers) is replicated, per-room
/// moderators start empty, and the power-levels event is owner-locked while
/// the tombstone is bot-only (150). The sender entry (pre-v12 rooms) is the
/// tombstone lock level so the bot can upgrade the room later.
pub fn initial_comment_room_power_levels(
    space_power_levels: &Value,
    sender_user_id: &str,
    include_sender: bool,
) -> Value {
    let mut users = Map::new();
    if include_sender {
        users.insert(
            sender_user_id.to_string(),
            Value::from(TOMBSTONE_LOCK_LEVEL),
        );
    }
    for entry in role_entries(space_power_levels, SITE_ROLE_MIN_LEVEL) {
        if entry.user_id != sender_user_id {
            users.insert(entry.user_id, Value::from(entry.level));
        }
    }
    serde_json::json!({
        "users": users,
        "events": {
            POWER_LEVELS_EVENT_TYPE: ROLE_LOCK_LEVEL,
            TOMBSTONE_EVENT_TYPE: TOMBSTONE_LOCK_LEVEL,
        },
    })
}

/// Replaces every site-managed entry (≥ [`SITE_ROLE_MIN_LEVEL`]) with the
/// given Space-derived list, while preserving the sender's own entry and all
/// per-room moderators (< 75). Also guarantees the governance locks.
pub fn reconcile_site_roles(
    power_levels: &Value,
    sender_user_id: &str,
    site_roles: &[RoleEntry],
) -> Value {
    let mut updated = power_levels.clone();
    let users = users_map_mut(&mut updated);
    users.retain(|user_id, level| {
        let Some(level) = level.as_i64() else {
            return true;
        };
        user_id == sender_user_id || level < SITE_ROLE_MIN_LEVEL
    });
    for role in site_roles {
        if role.user_id != sender_user_id {
            users.insert(role.user_id.clone(), Value::from(role.level));
        }
    }
    ensure_governance_locks(&updated)
}

/// Guarantees that `m.room.power_levels` is owner-locked and
/// `m.room.tombstone` is bot-only (150). Room version 12 requires the
/// tombstone threshold to be explicit and above `state_default`; without
/// this, a 50-level member could tombstone/upgrade a room, and with only
/// owner level a 100-level site owner could self-upgrade and inherit creator
/// power.
pub fn ensure_governance_locks(power_levels: &Value) -> Value {
    let mut updated = power_levels.clone();
    let events = updated
        .as_object_mut()
        .expect("power levels are an object")
        .entry("events")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("events is an object");
    events.insert(
        POWER_LEVELS_EVENT_TYPE.to_string(),
        Value::from(ROLE_LOCK_LEVEL),
    );
    events.insert(
        TOMBSTONE_EVENT_TYPE.to_string(),
        Value::from(TOMBSTONE_LOCK_LEVEL),
    );
    updated
}

/// Whether `user_id` may send `event_type` under the given power levels.
///
/// Mirrors the spec: `events[event_type]` overrides; state events otherwise
/// use `state_default` (default 50); a user's level is `users[user_id]` else
/// `users_default` (default 0). Membership events and redactions are not
/// covered here (they have their own thresholds).
pub fn can_send_state_event(power_levels: &Value, user_id: &str, event_type: &str) -> bool {
    let object = power_levels.as_object();
    let threshold = object
        .and_then(|o| o.get("events"))
        .and_then(Value::as_object)
        .and_then(|events| events.get(event_type))
        .and_then(Value::as_i64)
        .or_else(|| {
            object
                .and_then(|o| o.get("state_default"))
                .and_then(Value::as_i64)
        })
        .unwrap_or(MODERATOR_LEVEL);
    let level = object
        .and_then(|o| o.get("users"))
        .and_then(Value::as_object)
        .and_then(|users| users.get(user_id))
        .and_then(Value::as_i64)
        .or_else(|| {
            object
                .and_then(|o| o.get("users_default"))
                .and_then(Value::as_i64)
        })
        .unwrap_or(0);
    level >= threshold
}

/// Adds (or raises) one user to the given level.
pub fn with_user_level(power_levels: &Value, user_id: &str, level: i64) -> Value {
    let mut updated = power_levels.clone();
    users_map_mut(&mut updated).insert(user_id.to_string(), Value::from(level));
    updated
}

/// Removes one user's entry entirely.
pub fn without_user(power_levels: &Value, user_id: &str) -> Value {
    let mut updated = power_levels.clone();
    users_map_mut(&mut updated).remove(user_id);
    updated
}

/// Read-modify-write a single user's power level, shared by the governance API
/// and the reconciler's claim application so both sides converge identically.
pub async fn set_role_level(
    driver: &dyn MatrixDriver,
    room_id: &str,
    user_id: &str,
    level: i64,
    add: bool,
) -> anyhow::Result<Value> {
    let current = driver
        .get_room_power_levels(room_id)
        .await?
        .unwrap_or_else(|| serde_json::json!({}));
    let updated = if add {
        with_user_level(&current, user_id, level)
    } else {
        without_user(&current, user_id)
    };
    driver.set_room_power_levels(room_id, &updated).await?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn space() -> Value {
        json!({
            "users": { "@owner:hs": 100, "@co:hs": 75 },
            "events": { POWER_LEVELS_EVENT_TYPE: 100 },
            "state_default": 50,
        })
    }

    #[test]
    fn role_entries_filter_by_level_and_sort() {
        let pl = json!({
            "users": {
                "@b:hs": 50,
                "@a:hs": 75,
                "@c:hs": 100,
                "@d:hs": 0,
            }
        });
        assert_eq!(
            role_entries(&pl, SITE_ROLE_MIN_LEVEL),
            vec![
                RoleEntry {
                    user_id: "@a:hs".into(),
                    level: 75
                },
                RoleEntry {
                    user_id: "@c:hs".into(),
                    level: 100
                },
            ]
        );
    }

    #[test]
    fn comment_room_seeding_replicates_site_roles_and_locks() {
        let seeded = initial_comment_room_power_levels(&space(), "@bot:hs", true);
        assert_eq!(seeded["users"]["@owner:hs"], 100);
        assert_eq!(seeded["users"]["@co:hs"], 75);
        assert_eq!(
            seeded["users"]["@bot:hs"], TOMBSTONE_LOCK_LEVEL,
            "pre-v12 rooms must give the bot tombstone power"
        );
        assert_eq!(seeded["events"][POWER_LEVELS_EVENT_TYPE], 100);
        assert_eq!(seeded["events"][TOMBSTONE_EVENT_TYPE], TOMBSTONE_LOCK_LEVEL);
    }

    #[test]
    fn space_seeding_locks_tombstone_above_state_default() {
        let seeded = initial_space_power_levels("@bot:hs", true);
        assert_eq!(seeded["events"][POWER_LEVELS_EVENT_TYPE], 100);
        assert_eq!(seeded["events"][TOMBSTONE_EVENT_TYPE], TOMBSTONE_LOCK_LEVEL);
        assert!(
            seeded["events"][TOMBSTONE_EVENT_TYPE].as_i64().unwrap()
                > seeded["state_default"].as_i64().unwrap()
        );
    }

    #[test]
    fn reconcile_replaces_site_roles_but_preserves_moderators_and_sender() {
        let room = json!({
            "users": {
                "@bot:hs": 100,
                "@old-owner:hs": 100,
                "@old-co:hs": 75,
                "@mod:hs": 50,
            }
        });
        let reconciled = reconcile_site_roles(
            &room,
            "@bot:hs",
            &[RoleEntry {
                user_id: "@new-owner:hs".into(),
                level: 100,
            }],
        );
        assert_eq!(
            reconciled,
            json!({
                "users": {
                    "@bot:hs": 100,
                    "@mod:hs": 50,
                    "@new-owner:hs": 100,
                },
                "events": {
                    POWER_LEVELS_EVENT_TYPE: 100,
                    TOMBSTONE_EVENT_TYPE: TOMBSTONE_LOCK_LEVEL,
                },
            })
        );
    }

    #[test]
    fn governance_locks_add_missing_tombstone_threshold() {
        let legacy = json!({
            "users": { "@owner:hs": 100 },
            "events": { POWER_LEVELS_EVENT_TYPE: 100 },
            "state_default": 50,
        });
        let locked = ensure_governance_locks(&legacy);
        assert_eq!(locked["events"][TOMBSTONE_EVENT_TYPE], TOMBSTONE_LOCK_LEVEL);
        // Idempotent: re-running keeps both locks.
        assert_eq!(ensure_governance_locks(&locked), locked);
    }

    #[test]
    fn with_and_without_user_edit_single_entries() {
        let pl = json!({ "users": { "@a:hs": 50 } });
        assert_eq!(with_user_level(&pl, "@b:hs", 50)["users"]["@b:hs"], 50);
        assert!(without_user(&pl, "@a:hs")["users"].get("@a:hs").is_none());
    }

    #[test]
    fn as_managed_users_are_detected_by_localpart_prefix() {
        assert!(is_as_managed_user("@_cumments_bot:hs"));
        assert!(is_as_managed_user(
            "@_cumments_a_3282f2a21b4a1e6b3282f2a21b4a1e6b:hs"
        ));
        assert!(!is_as_managed_user("@owner:hs"));
        assert!(!is_as_managed_user("not-an-mxid"));
    }

    #[test]
    fn can_send_state_event_follows_spec_thresholds() {
        let pl = json!({
            "users": { "@owner:hs": 100, "@co:hs": 75, "@guest:hs": 0 },
            "events": { POWER_LEVELS_EVENT_TYPE: 100 },
            "state_default": 50,
            "users_default": 0,
        });
        assert!(can_send_state_event(
            &pl,
            "@owner:hs",
            POWER_LEVELS_EVENT_TYPE
        ));
        assert!(!can_send_state_event(
            &pl,
            "@co:hs",
            POWER_LEVELS_EVENT_TYPE
        ));
        // Stickers use state_default unless overridden.
        assert!(can_send_state_event(&pl, "@co:hs", "m.room.image_pack"));
        assert!(!can_send_state_event(&pl, "@guest:hs", "m.room.image_pack"));
        // users_default supplies unlisted users.
        assert!(!can_send_state_event(
            &pl,
            "@stranger:hs",
            "m.room.image_pack"
        ));
        let open = json!({ "users_default": 50 });
        assert!(can_send_state_event(
            &open,
            "@stranger:hs",
            "m.room.image_pack"
        ));
        // Above-owner levels can send governance too.
        let elevated = json!({ "users": { "@admin:hs": 150 } });
        assert!(can_send_state_event(
            &elevated,
            "@admin:hs",
            POWER_LEVELS_EVENT_TYPE
        ));
        // Missing PL content falls back to state_default 50 / users_default 0.
        assert!(!can_send_state_event(
            &json!({}),
            "@anyone:hs",
            "m.room.image_pack"
        ));
        // A 50-level moderator and a 100-level site owner cannot send the
        // tombstone; only a user at the 150 lock level (the bot creator, or
        // someone explicitly boosted) can.
        let locked = json!({
            "users": { "@owner:hs": 100, "@mod:hs": 50, "@elevated:hs": 150 },
            "events": {
                POWER_LEVELS_EVENT_TYPE: 100,
                TOMBSTONE_EVENT_TYPE: TOMBSTONE_LOCK_LEVEL,
            },
            "state_default": 50,
        });
        assert!(!can_send_state_event(
            &locked,
            "@mod:hs",
            TOMBSTONE_EVENT_TYPE
        ));
        assert!(!can_send_state_event(
            &locked,
            "@owner:hs",
            TOMBSTONE_EVENT_TYPE
        ));
        assert!(can_send_state_event(
            &locked,
            "@elevated:hs",
            TOMBSTONE_EVENT_TYPE
        ));
    }
}
