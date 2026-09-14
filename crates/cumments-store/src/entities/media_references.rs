use sea_orm::entity::prelude::*;

/// Durable site-scoped mapping between a stable [`cumments_core::media_reference::MediaReference`]
/// and an underlying Matrix homeserver MXC URI.
///
/// Survives disposable projection rebuilds. Uniqueness is site-scoped (`UNIQUE(site_id, mxc_uri)`),
/// not globally unique across all sites.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "media_references")]
pub struct Model {
    /// Canonical textual representation of the media reference (`cumments-media:<uuid>`).
    #[sea_orm(primary_key, auto_increment = false)]
    pub media_reference: String,
    /// Site scope of the media reference.
    pub site_id: String,
    /// Underlying Matrix homeserver MXC URI (`mxc://server/media_id`).
    pub mxc_uri: String,
    /// Whether this media reference was discovered via external Matrix avatar.
    pub is_external: bool,
    /// Timestamp when this reference mapping was created.
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
