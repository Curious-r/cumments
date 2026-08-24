use sea_orm::entity::prelude::*;

/// A small projection of the homeserver's resolved current state. Historical
/// event rows remain in `room_state_events`; this table is what redaction
/// should consult for the room's actual version.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "room_state_snapshots")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub room_id: String,
    pub room_version: Option<String>,
    pub create_content_json: Option<String>,
    pub power_levels_json: Option<String>,
    pub resolved_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
