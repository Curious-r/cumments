use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "media_uploads")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// MXC URI returned by the upload endpoint.
    #[sea_orm(unique)]
    pub mxc_url: String,
    /// Visitor public key that uploaded the media.
    pub author_public_key: String,
    /// Site the upload was authorized for; the post when the upload is
    /// comment-scoped, `None` for site-scoped identity media (avatars).
    pub site_id: String,
    pub page_slug: Option<String>,
    /// When a comment submission referencing this media was queued/sent.
    /// Historical upload bookkeeping only; never a reachability or age signal.
    pub used_at: Option<DateTimeUtc>,
    /// The post submission that currently references this media, if any.
    pub submission_id: Option<i64>,
    pub created_at: DateTimeUtc,
}

impl ActiveModelBehavior for ActiveModel {}
