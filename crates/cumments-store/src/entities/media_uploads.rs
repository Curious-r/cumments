use sea_orm::entity::prelude::*;

/// Local record that a visitor uploaded an MXC through the authorized upload
/// path for a site/page, so a later comment write can prove the media came
/// through that path.
///
/// This is Cumments-side upload provenance and write admission only. It is not
/// ownership of the Matrix media object: the homeserver owns and retains the
/// actual media, and Cumments never deletes it.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "media_uploads")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// MXC URI returned by the upload endpoint.
    ///
    /// Deliberately not globally unique: upload records are scoped to
    /// `UNIQUE(site_id, mxc_url)`, so the same MXC may be recorded by several
    /// sites at once.
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
