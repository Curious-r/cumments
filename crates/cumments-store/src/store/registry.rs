use super::DbStore;
use crate::entities::active_enums::{SiteAuthMode, SiteVerificationStatus};
use crate::entities::{room_registry, room_upgrade_intents, sites};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use cumments_core::models::{
    PageSlug, QuarantinedRoom, RoomIdentity, RoomStatus, RoomUpgradeIntent,
    RoomUpgradeIntentStatus, Site, SiteId,
};
use cumments_core::ports::{RegistryStore, SiteStore};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};

#[async_trait]
impl RegistryStore for DbStore {
    async fn get_registered_room(
        &self,
        site_id: &SiteId,
        page_slug: &PageSlug,
    ) -> Result<Option<String>> {
        let room = room_registry::Entity::find()
            .filter(room_registry::COLUMN.site_id.eq(site_id.as_str()))
            .filter(room_registry::COLUMN.page_slug.eq(page_slug.as_str()))
            .filter(room_registry::COLUMN.status.eq(RoomStatus::Active.as_str()))
            .one(&self.db)
            .await?;

        Ok(room.map(|r| r.room_id))
    }

    async fn get_room_status(&self, room_id: &str) -> Result<Option<RoomStatus>> {
        let room = room_registry::Entity::find_by_id(room_id.to_owned())
            .one(&self.db)
            .await?;

        room.map(|r| {
            r.status.parse::<RoomStatus>().map_err(|e| {
                anyhow!(
                    "invalid room status `{}` for room {}: {e}",
                    r.status,
                    room_id
                )
            })
        })
        .transpose()
    }

    async fn list_active_rooms(&self) -> Result<Vec<String>> {
        let rows = room_registry::Entity::find()
            .filter(room_registry::COLUMN.status.eq(RoomStatus::Active.as_str()))
            .all(&self.db)
            .await?;
        Ok(rows.into_iter().map(|row| row.room_id).collect())
    }

    async fn list_active_rooms_for_site(&self, site_id: &SiteId) -> Result<Vec<String>> {
        let rows = room_registry::Entity::find()
            .filter(room_registry::COLUMN.site_id.eq(site_id.as_str()))
            .filter(room_registry::COLUMN.status.eq(RoomStatus::Active.as_str()))
            .all(&self.db)
            .await?;
        Ok(rows.into_iter().map(|row| row.room_id).collect())
    }

    async fn list_rooms_for_site(&self, site_id: &SiteId) -> Result<Vec<String>> {
        let rows = room_registry::Entity::find()
            .filter(room_registry::COLUMN.site_id.eq(site_id.as_str()))
            .all(&self.db)
            .await?;
        Ok(rows.into_iter().map(|row| row.room_id).collect())
    }

    async fn list_superseded_rooms(&self) -> Result<Vec<String>> {
        let rows = room_registry::Entity::find()
            .filter(
                room_registry::COLUMN
                    .status
                    .eq(RoomStatus::Superseded.as_str()),
            )
            .all(&self.db)
            .await?;
        Ok(rows.into_iter().map(|row| row.room_id).collect())
    }

    async fn list_retired_rooms(&self) -> Result<Vec<String>> {
        let rows = room_registry::Entity::find()
            .filter(
                room_registry::COLUMN
                    .status
                    .eq(RoomStatus::Retired.as_str()),
            )
            .all(&self.db)
            .await?;
        Ok(rows.into_iter().map(|row| row.room_id).collect())
    }

    async fn get_registered_room_identity(&self, room_id: &str) -> Result<Option<RoomIdentity>> {
        let room = room_registry::Entity::find_by_id(room_id.to_owned())
            .one(&self.db)
            .await?;

        Ok(room.map(|r| RoomIdentity {
            site_id: r.site_id,
            page_slug: r.page_slug,
        }))
    }

    async fn register_room(
        &self,
        room_id: &str,
        site_id: &SiteId,
        page_slug: &PageSlug,
    ) -> Result<()> {
        let txn = self.db.begin().await?;

        // Enforce a single active room per (site_id, page_slug): supersede
        // any other active rows before activating the new room.
        room_registry::Entity::update_many()
            .col_expr(
                room_registry::Column::Status,
                sea_orm::sea_query::Expr::value(RoomStatus::Superseded.as_str()),
            )
            .col_expr(
                room_registry::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(room_registry::COLUMN.site_id.eq(site_id.as_str()))
            .filter(room_registry::COLUMN.page_slug.eq(page_slug.as_str()))
            .filter(room_registry::COLUMN.status.eq(RoomStatus::Active.as_str()))
            .filter(room_registry::COLUMN.room_id.ne(room_id))
            .exec(&txn)
            .await?;

        let active_model = room_registry::ActiveModel {
            room_id: Set(room_id.to_owned()),
            site_id: Set(site_id.as_str().to_owned()),
            page_slug: Set(page_slug.as_str().to_owned()),
            status: Set(RoomStatus::Active.as_str().to_owned()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            quarantine_reason: Set(None),
            quarantined_at: Set(None),
            adoption_failures: Set(0),
            next_attempt_at: Set(None),
        };

        room_registry::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(room_registry::Column::RoomId)
                    .update_column(room_registry::Column::Status)
                    .update_column(room_registry::Column::SiteId)
                    .update_column(room_registry::Column::PageSlug)
                    .update_column(room_registry::Column::UpdatedAt)
                    .update_column(room_registry::Column::QuarantineReason)
                    .update_column(room_registry::Column::QuarantinedAt)
                    .update_column(room_registry::Column::AdoptionFailures)
                    .update_column(room_registry::Column::NextAttemptAt)
                    .to_owned(),
            )
            .exec(&txn)
            .await?;

        txn.commit().await?;

        Ok(())
    }

    async fn register_room_if_absent(
        &self,
        room_id: &str,
        site_id: &SiteId,
        page_slug: &PageSlug,
    ) -> Result<()> {
        let active_model = room_registry::ActiveModel {
            room_id: Set(room_id.to_owned()),
            site_id: Set(site_id.as_str().to_owned()),
            page_slug: Set(page_slug.as_str().to_owned()),
            status: Set(RoomStatus::Active.as_str().to_owned()),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            quarantine_reason: Set(None),
            quarantined_at: Set(None),
            adoption_failures: Set(0),
            next_attempt_at: Set(None),
        };

        room_registry::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(room_registry::Column::RoomId)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(&self.db)
            .await?;

        Ok(())
    }

    async fn retire_room(&self, room_id: &str) -> Result<()> {
        room_registry::Entity::update_many()
            .col_expr(
                room_registry::Column::Status,
                sea_orm::sea_query::Expr::value(RoomStatus::Superseded.as_str()),
            )
            .col_expr(
                room_registry::Column::NextAttemptAt,
                sea_orm::sea_query::Expr::value(None::<chrono::DateTime<chrono::Utc>>),
            )
            .col_expr(
                room_registry::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(room_registry::COLUMN.room_id.eq(room_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn mark_room_retired(&self, room_id: &str) -> Result<bool> {
        let result = room_registry::Entity::update_many()
            .col_expr(
                room_registry::Column::Status,
                sea_orm::sea_query::Expr::value(RoomStatus::Retired.as_str()),
            )
            .col_expr(
                room_registry::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(room_registry::COLUMN.room_id.eq(room_id))
            .filter(room_registry::COLUMN.status.eq(RoomStatus::Active.as_str()))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected > 0)
    }

    async fn quarantine_room(
        &self,
        room_id: &str,
        reason: &str,
        adoption_failures: u32,
        next_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()> {
        let now = chrono::Utc::now();
        let Some(model) = room_registry::Entity::find_by_id(room_id.to_owned())
            .one(&self.db)
            .await?
        else {
            return Ok(());
        };
        let quarantined_at = if model.status == RoomStatus::Quarantined.as_str() {
            model.quarantined_at
        } else {
            Some(now)
        };

        room_registry::Entity::update_many()
            .col_expr(
                room_registry::Column::Status,
                sea_orm::sea_query::Expr::value(RoomStatus::Quarantined.as_str()),
            )
            .col_expr(
                room_registry::Column::QuarantineReason,
                sea_orm::sea_query::Expr::value(Some(reason)),
            )
            .col_expr(
                room_registry::Column::QuarantinedAt,
                sea_orm::sea_query::Expr::value(quarantined_at),
            )
            .col_expr(
                room_registry::Column::AdoptionFailures,
                sea_orm::sea_query::Expr::value(adoption_failures),
            )
            .col_expr(
                room_registry::Column::NextAttemptAt,
                sea_orm::sea_query::Expr::value(next_attempt_at),
            )
            .col_expr(
                room_registry::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(room_registry::COLUMN.room_id.eq(room_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    async fn reinstate_room(&self, room_id: &str) -> Result<bool> {
        let txn = self.db.begin().await?;
        let Some(model) = room_registry::Entity::find_by_id(room_id.to_owned())
            .one(&txn)
            .await?
        else {
            return Ok(false);
        };

        // Enforce the single-active-room invariant: supersede any other
        // active room for the same site/post before activating this one.
        room_registry::Entity::update_many()
            .col_expr(
                room_registry::Column::Status,
                sea_orm::sea_query::Expr::value(RoomStatus::Superseded.as_str()),
            )
            .col_expr(
                room_registry::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(chrono::Utc::now()),
            )
            .filter(room_registry::COLUMN.site_id.eq(model.site_id.as_str()))
            .filter(room_registry::COLUMN.page_slug.eq(model.page_slug.as_str()))
            .filter(room_registry::COLUMN.status.eq(RoomStatus::Active.as_str()))
            .filter(room_registry::COLUMN.room_id.ne(room_id))
            .exec(&txn)
            .await?;

        let now = chrono::Utc::now();
        room_registry::Entity::update_many()
            .col_expr(
                room_registry::Column::Status,
                sea_orm::sea_query::Expr::value(RoomStatus::Active.as_str()),
            )
            .col_expr(
                room_registry::Column::QuarantineReason,
                sea_orm::sea_query::Expr::value(None::<String>),
            )
            .col_expr(
                room_registry::Column::QuarantinedAt,
                sea_orm::sea_query::Expr::value(None::<chrono::DateTime<chrono::Utc>>),
            )
            .col_expr(
                room_registry::Column::AdoptionFailures,
                sea_orm::sea_query::Expr::value(0u32),
            )
            .col_expr(
                room_registry::Column::NextAttemptAt,
                sea_orm::sea_query::Expr::value(None::<chrono::DateTime<chrono::Utc>>),
            )
            .col_expr(
                room_registry::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(room_registry::COLUMN.room_id.eq(room_id))
            .exec(&txn)
            .await?;
        txn.commit().await?;
        Ok(true)
    }

    async fn get_quarantined_rooms(&self) -> Result<Vec<QuarantinedRoom>> {
        let models = room_registry::Entity::find()
            .filter(room_registry::Column::Status.eq(RoomStatus::Quarantined.as_str()))
            .all(&self.db)
            .await?;
        models
            .into_iter()
            .map(|m| {
                let quarantine_reason = m.quarantine_reason.ok_or_else(|| {
                    anyhow!("quarantined room {} has no quarantine_reason", m.room_id)
                })?;
                Ok(QuarantinedRoom {
                    room_id: m.room_id,
                    site_id: m.site_id,
                    page_slug: m.page_slug,
                    quarantine_reason,
                    quarantined_at: m.quarantined_at.unwrap_or(m.updated_at),
                    adoption_failures: m.adoption_failures,
                    next_attempt_at: m.next_attempt_at,
                })
            })
            .collect()
    }

    async fn record_upgrade_intent(
        &self,
        old_room_id: &str,
        new_version: &str,
    ) -> Result<RoomUpgradeIntent> {
        let now = chrono::Utc::now();
        let Some(model) = room_upgrade_intents::Entity::find_by_id(old_room_id.to_owned())
            .one(&self.db)
            .await?
        else {
            let active_model = room_upgrade_intents::ActiveModel {
                old_room_id: Set(old_room_id.to_owned()),
                expected_new_version: Set(new_version.to_owned()),
                replacement_room_id: Set(None),
                status: Set(RoomUpgradeIntentStatus::Requested.as_str().to_owned()),
                error_message: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            };
            room_upgrade_intents::Entity::insert(active_model)
                .exec(&self.db)
                .await?;
            return Ok(RoomUpgradeIntent {
                old_room_id: old_room_id.to_owned(),
                expected_new_version: new_version.to_owned(),
                replacement_room_id: None,
                status: RoomUpgradeIntentStatus::Requested,
                error_message: None,
                created_at: now,
                updated_at: now,
            });
        };

        // Terminal audit states are never silently rewritten. Failed intents
        // may be reopened by an explicit operator/API retry; adopted upgrades
        // require a new successor (and therefore a new old-room intent).
        let status = model
            .status
            .parse::<RoomUpgradeIntentStatus>()
            .map_err(|e| anyhow!("{e}"))?;
        if matches!(
            status,
            RoomUpgradeIntentStatus::Manual | RoomUpgradeIntentStatus::Adopted
        ) {
            return model_to_intent(model);
        }

        room_upgrade_intents::Entity::update_many()
            .col_expr(
                room_upgrade_intents::Column::ExpectedNewVersion,
                sea_orm::sea_query::Expr::value(new_version),
            )
            .col_expr(
                room_upgrade_intents::Column::ReplacementRoomId,
                sea_orm::sea_query::Expr::value(None::<String>),
            )
            .col_expr(
                room_upgrade_intents::Column::Status,
                sea_orm::sea_query::Expr::value(RoomUpgradeIntentStatus::Requested.as_str()),
            )
            .col_expr(
                room_upgrade_intents::Column::ErrorMessage,
                sea_orm::sea_query::Expr::value(None::<String>),
            )
            .col_expr(
                room_upgrade_intents::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(room_upgrade_intents::COLUMN.old_room_id.eq(old_room_id))
            .exec(&self.db)
            .await?;

        Ok(RoomUpgradeIntent {
            old_room_id: old_room_id.to_owned(),
            expected_new_version: new_version.to_owned(),
            replacement_room_id: None,
            status: RoomUpgradeIntentStatus::Requested,
            error_message: None,
            created_at: model.created_at,
            updated_at: now,
        })
    }

    async fn observe_upgrade_replacement(
        &self,
        old_room_id: &str,
        replacement_room_id: &str,
    ) -> Result<Option<RoomUpgradeIntent>> {
        update_open_intent(self, old_room_id, |intent| {
            if intent
                .replacement_room_id
                .as_ref()
                .is_some_and(|id| id != replacement_room_id)
            {
                intent.status = RoomUpgradeIntentStatus::Failed;
                intent.error_message = Some(format!(
                    "replacement changed from {:?} to {replacement_room_id:?}",
                    intent.replacement_room_id
                ));
            } else {
                intent.replacement_room_id = Some(replacement_room_id.to_owned());
                intent.status = RoomUpgradeIntentStatus::Observed;
                intent.error_message = None;
            }
        })
        .await
    }

    async fn complete_upgrade_intent(
        &self,
        old_room_id: &str,
        replacement_room_id: &str,
    ) -> Result<Option<RoomUpgradeIntent>> {
        update_open_intent(self, old_room_id, |intent| {
            if intent.replacement_room_id.as_deref() == Some(replacement_room_id) {
                intent.status = RoomUpgradeIntentStatus::Adopted;
                intent.error_message = None;
            }
        })
        .await
    }

    async fn fail_upgrade_intent(&self, old_room_id: &str, reason: &str) -> Result<()> {
        update_open_intent(self, old_room_id, |intent| {
            intent.status = RoomUpgradeIntentStatus::Failed;
            intent.error_message = Some(reason.to_owned());
        })
        .await?;
        Ok(())
    }

    async fn mark_upgrade_intent_manual(&self, old_room_id: &str, reason: &str) -> Result<()> {
        update_open_intent(self, old_room_id, |intent| {
            intent.status = RoomUpgradeIntentStatus::Manual;
            intent.error_message = Some(reason.to_owned());
        })
        .await?;
        Ok(())
    }

    async fn get_upgrade_intent(&self, old_room_id: &str) -> Result<Option<RoomUpgradeIntent>> {
        let model = room_upgrade_intents::Entity::find_by_id(old_room_id.to_owned())
            .one(&self.db)
            .await?;
        model.map(model_to_intent).transpose()
    }
}

fn model_to_intent(model: room_upgrade_intents::Model) -> Result<RoomUpgradeIntent> {
    Ok(RoomUpgradeIntent {
        old_room_id: model.old_room_id,
        expected_new_version: model.expected_new_version,
        replacement_room_id: model.replacement_room_id,
        status: model.status.parse().map_err(|e| anyhow!("{e}"))?,
        error_message: model.error_message,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

/// Applies a state transition to every non-terminal state. Keeping the filter
/// explicit prevents a delayed response from overwriting a later manual review.
async fn update_open_intent(
    store: &DbStore,
    old_room_id: &str,
    transition: impl FnOnce(&mut RoomUpgradeIntent),
) -> Result<Option<RoomUpgradeIntent>> {
    let Some(model) = room_upgrade_intents::Entity::find_by_id(old_room_id.to_owned())
        .one(&store.db)
        .await?
    else {
        return Ok(None);
    };
    let mut intent = model_to_intent(model)?;
    if matches!(
        intent.status,
        RoomUpgradeIntentStatus::Observed
            | RoomUpgradeIntentStatus::Requested
            | RoomUpgradeIntentStatus::Failed
    ) {
        transition(&mut intent);
        let now = chrono::Utc::now();
        room_upgrade_intents::Entity::update_many()
            .col_expr(
                room_upgrade_intents::Column::ReplacementRoomId,
                sea_orm::sea_query::Expr::value(intent.replacement_room_id.clone()),
            )
            .col_expr(
                room_upgrade_intents::Column::Status,
                sea_orm::sea_query::Expr::value(intent.status.as_str()),
            )
            .col_expr(
                room_upgrade_intents::Column::ErrorMessage,
                sea_orm::sea_query::Expr::value(intent.error_message.clone()),
            )
            .col_expr(
                room_upgrade_intents::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(room_upgrade_intents::COLUMN.old_room_id.eq(old_room_id))
            .filter(
                room_upgrade_intents::COLUMN
                    .status
                    .ne(RoomUpgradeIntentStatus::Adopted.as_str()),
            )
            .filter(
                room_upgrade_intents::COLUMN
                    .status
                    .ne(RoomUpgradeIntentStatus::Manual.as_str()),
            )
            .exec(&store.db)
            .await?;
        intent.updated_at = now;
    }
    Ok(Some(intent))
}

#[async_trait]
impl SiteStore for DbStore {
    async fn get_site(&self, id: &SiteId) -> Result<Option<Site>> {
        let model = sites::Entity::find_by_id(id.as_str().to_owned())
            .one(&self.db)
            .await?;

        Ok(model.map(Site::from))
    }

    async fn get_site_by_space_id(&self, space_id: &str) -> Result<Option<Site>> {
        let model = sites::Entity::find()
            .filter(sites::COLUMN.matrix_space_id.eq(space_id))
            .one(&self.db)
            .await?;

        Ok(model.map(Site::from))
    }

    async fn list_sites(&self) -> Result<Vec<Site>> {
        let models = sites::Entity::find().all(&self.db).await?;
        Ok(models.into_iter().map(Site::from).collect())
    }

    async fn save_site(&self, site: &Site) -> Result<()> {
        let now = chrono::Utc::now();
        let active_model = sites::ActiveModel {
            id: Set(site.id.clone()),
            matrix_space_id: Set(site.matrix_space_id.clone()),
            display_name: Set(site.display_name.clone()),
            auth_mode: Set(SiteAuthMode::Origin),
            verification_status: Set(SiteVerificationStatus::Unverified),
            claim_token_hash: Set(None),
            is_custom_id: Set(false),
            lifecycle_status: Set(crate::entities::active_enums::SiteLifecycleStatus::Active),
            secret: Set(None),
            verified_at: Set(None),
            created_at: Set(site.created_at),
            updated_at: Set(Some(now)),
        };

        sites::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(sites::Column::Id)
                    .update_columns([sites::Column::MatrixSpaceId, sites::Column::DisplayName])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }

    async fn ensure_site_exists(&self, site_id: &str, matrix_space_id: &str) -> Result<()> {
        let now = chrono::Utc::now();
        let active_model = sites::ActiveModel {
            id: Set(site_id.to_owned()),
            matrix_space_id: Set(matrix_space_id.to_owned()),
            display_name: Set(Some(site_id.to_owned())),
            auth_mode: Set(SiteAuthMode::Origin),
            verification_status: Set(SiteVerificationStatus::Unverified),
            claim_token_hash: Set(None),
            is_custom_id: Set(false),
            lifecycle_status: Set(crate::entities::active_enums::SiteLifecycleStatus::Active),
            secret: Set(None),
            verified_at: Set(None),
            created_at: Set(now),
            updated_at: Set(Some(now)),
        };

        sites::Entity::insert(active_model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(sites::Column::Id)
                    // Upsert instead of do-nothing: with DO NOTHING and no
                    // RETURNING row, sea-orm treats a conflicting existing
                    // site as an insert failure, which made every space-child
                    // push event fail and blocked the homeserver's push queue.
                    .update_columns([sites::Column::MatrixSpaceId, sites::Column::DisplayName])
                    .to_owned(),
            )
            .exec(&self.db)
            .await?;

        Ok(())
    }
}

impl From<sites::Model> for Site {
    fn from(model: sites::Model) -> Self {
        Site {
            id: model.id,
            matrix_space_id: model.matrix_space_id,
            display_name: model.display_name,
            created_at: model.created_at,
        }
    }
}
