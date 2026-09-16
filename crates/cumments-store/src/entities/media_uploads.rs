use sea_orm::entity::prelude::*;

/// Local record that a visitor uploaded an MXC through the authorized upload
/// path for a given scope, so a later write can prove the media came through
/// that path.
///
/// Each row is one provenance fact `(site, visitor, scope, MXC)`: comment media
/// is page-scoped while a site-scoped avatar has no page. The unique key covers
/// the whole fact, so the same MXC may legitimately appear in several rows when
/// a homeserver returns an existing media object for a later upload.
///
/// This is Cumments-side upload provenance and write admission only. It is not
/// ownership of the Matrix media object: Matrix media lifetime is the
/// homeserver's responsibility, and Cumments never deletes media from it.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "media_uploads")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// MXC URI returned by the upload endpoint.
    ///
    /// Deliberately not unique on its own. Matrix does not guarantee that an
    /// MXC URI originates from exactly one upload, so the uniqueness is defined
    /// over the full provenance fact by the table's partial unique indexes
    /// rather than by this column.
    pub mxc_url: String,
    /// Visitor public key that uploaded the media.
    pub author_public_key: String,
    /// Site the upload was authorized for; the page when the upload is
    /// comment-scoped, `None` for site-scoped identity media (avatars).
    pub site_id: String,
    pub page_slug: Option<String>,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
