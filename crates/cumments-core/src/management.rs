//! Management use cases shared by every adapter (CLI, API, bot).
//!
//! Adapters own authentication/authorization and response formatting; each
//! operation is implemented exactly once here on top of the core ports.

use crate::governance::{
    MANAGER_LEVEL, MODERATOR_LEVEL, NewRoleClaim, SITE_ADMIN_LEVEL, SITE_ROLE_MIN_LEVEL,
    SiteTransfer, role_entries, set_role_level, validate_governance_user_id,
};
use crate::models::{PageSlug, RoomStatus, SiteId};
use crate::ports::{
    GovernanceStore, MatrixDriver, RegistryStore, RoleClaimStore, SiteAuthStore, SiteTransferStore,
};
use crate::site_auth::{SiteAuthPolicy, generate_token, token_hash};
use crate::site_service::SiteService;
use chrono::{Duration, Utc};
use std::collections::HashSet;
use tracing::warn;

/// How long an unverified role claim stays valid.
pub const ROLE_CLAIM_TTL_HOURS: i64 = 24;

/// Domain failures from the shared management use cases.
///
/// Adapters map these to their own surfaces: the HTTP layer turns
/// [`ManagementError::RoleNotFound`] into 404 and
/// [`ManagementError::SiteLevelRoleConflict`] into 409, while the bot and CLI
/// render the display text.
#[derive(Debug, thiserror::Error)]
pub enum ManagementError {
    #[error("invalid user id: {0}")]
    InvalidUserId(String),
    #[error("invalid site id: {0}")]
    InvalidSiteId(String),
    #[error("no pending claim or applied role for this user and level")]
    RoleNotFound,
    #[error(
        "this user holds a site-level role; manage them through the admins or managers endpoint"
    )]
    SiteLevelRoleConflict,
    #[error("room {0} is not in the registry")]
    RoomNotRegistered(String),
    #[error("room {0} is not active (quarantined or superseded)")]
    RoomNotActive(String),
    #[error("invalid Matrix room version `{0}`")]
    InvalidRoomVersion(String),
    #[error("invalid page slug `{0}`")]
    InvalidPageSlug(String),
    #[error("target room version {1} is not newer than the current version {0}")]
    RoomVersionNotNewer(String, String),
    #[error("room {0} has no m.room.create event")]
    RoomWithoutCreateEvent(String),
    #[error("site {0} is not API-registered; ownership transfer requires a claim token")]
    SiteNotApiRegistered(String),
    #[error(transparent)]
    Infra(#[from] anyhow::Error),
}

/// Whether `value` is a syntactically valid Matrix room version identifier
/// (1-32 chars from a-z, 0-9, '.', '-'), mirroring the config validator.
pub fn is_valid_room_version(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 32
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-'))
}

/// Upgrades one registered active comment room through the homeserver's
/// native `/upgrade` and converges the replacement into Cumments.
///
/// The native upgrade does not copy our metadata, does not update external
/// Space references (MSC4168 is open and tuwunel only copies space state into
/// the new room), and does not transfer membership, so this use case owns
/// those writes: adoption + metadata repair, Space child re-link, old-child
/// `via` clearing (best-effort), site-role re-invites, and registry
/// activation (which supersedes the old room). Returns the replacement room
/// ID. The target version must be newer than the current version (Matrix
/// itself does not forbid downgrades, so this is an application policy).
/// Pre-v12 rooms are supported when the bot holds tombstone power: new
/// Cumments rooms grant the bot 150, while legacy pre-v12 rooms whose bot is
/// only 100 cannot be upgraded and are an accepted breaking change.
pub async fn upgrade_comment_room(
    driver: &dyn MatrixDriver,
    registry: &dyn RegistryStore,
    site_service: &SiteService,
    room_id: &str,
    new_version: &str,
) -> Result<String, ManagementError> {
    if !is_valid_room_version(new_version) {
        return Err(ManagementError::InvalidRoomVersion(new_version.to_string()));
    }
    let identity = registry
        .get_registered_room_identity(room_id)
        .await?
        .ok_or_else(|| ManagementError::RoomNotRegistered(room_id.to_string()))?;
    if registry.get_room_status(room_id).await? != Some(RoomStatus::Active) {
        return Err(ManagementError::RoomNotActive(room_id.to_string()));
    }
    let create = driver
        .get_room_state(room_id, "m.room.create", "")
        .await?
        .ok_or_else(|| ManagementError::RoomWithoutCreateEvent(room_id.to_string()))?;
    let version = create
        .get("room_version")
        .and_then(|v| v.as_str())
        .unwrap_or("1");
    let Some(current_major) = version
        .split(['.', '-'])
        .next()
        .and_then(|v| v.parse::<u32>().ok())
    else {
        return Err(ManagementError::InvalidRoomVersion(version.to_string()));
    };
    let Some(target_major) = new_version
        .split(['.', '-'])
        .next()
        .and_then(|v| v.parse::<u32>().ok())
    else {
        return Err(ManagementError::InvalidRoomVersion(new_version.to_string()));
    };
    if target_major <= current_major {
        return Err(ManagementError::RoomVersionNotNewer(
            version.to_string(),
            new_version.to_string(),
        ));
    }
    let site_id = SiteId::new(identity.site_id.clone())
        .map_err(|e| ManagementError::InvalidSiteId(e.to_string()))?;
    let page_slug = PageSlug::new(identity.page_slug.clone())
        .map_err(|e| ManagementError::InvalidPageSlug(e.to_string()))?;
    let space_id = site_service.ensure_space(&site_id, driver).await?;

    let replacement = driver.upgrade_room(room_id, new_version).await?;

    driver
        .adopt_room(&replacement, &site_id, Some(&page_slug), false)
        .await?;
    driver.link_room_to_space(&space_id, &replacement).await?;

    // Best-effort: clear the old child's `via` so clients stop treating the
    // tombstoned room as part of the Space (MSC4168 semantics).
    if let Err(e) = driver
        .set_room_state(&space_id, "m.space.child", room_id, &serde_json::json!({}))
        .await
    {
        warn!("failed to clear old Space child {room_id}: {e:#}");
    }

    let sender = driver.sender_user_id().unwrap_or_default();
    if let Some(space_power_levels) = driver.get_room_power_levels(&space_id).await? {
        for role in role_entries(&space_power_levels, SITE_ROLE_MIN_LEVEL) {
            if role.user_id == sender {
                continue;
            }
            if let Err(e) = driver.invite_user(&replacement, &role.user_id).await {
                warn!(
                    "failed to re-invite {} to upgraded room: {e:#}",
                    role.user_id
                );
            }
        }
    }

    registry
        .register_room(&replacement, &site_id, &page_slug)
        .await?;
    Ok(replacement)
}

/// Site-owner entry point for [`upgrade_comment_room`]: resolves the active
/// room for `(site_id, page_slug)` from the registry, then runs the same
/// upgrade and convergence. Shared by the claim-token API and the bot's
/// site-level command; the operator mirror continues to take a raw room ID.
pub async fn upgrade_site_page_room(
    driver: &dyn MatrixDriver,
    registry: &dyn RegistryStore,
    site_service: &SiteService,
    site_id: &SiteId,
    page_slug: &PageSlug,
    new_version: &str,
) -> Result<String, ManagementError> {
    let room_id = registry
        .get_registered_room(site_id, page_slug)
        .await?
        .ok_or_else(|| {
            ManagementError::RoomNotRegistered(format!(
                "{}/{}",
                site_id.as_str(),
                page_slug.as_str()
            ))
        })?;
    upgrade_comment_room(driver, registry, site_service, &room_id, new_version).await
}

/// A pending token-DM role claim created by [`create_role_claim`].
#[derive(Debug, Clone)]
pub struct PendingRoleClaim {
    pub user_id: String,
    pub level: i64,
    pub verify_token: String,
    pub expires_at: chrono::DateTime<Utc>,
}

/// What a removal actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleRemoval {
    /// A pending (or activated) claim was cancelled before it reached Matrix.
    PendingRevoked,
    /// The role was already applied; it was removed from Matrix power levels.
    AppliedRemoved,
}

/// Creates or rotates a pending token-DM claim for a role.
pub async fn create_role_claim(
    store: &dyn RoleClaimStore,
    site_id: &str,
    room_id: &str,
    user_id: &str,
    level: i64,
) -> Result<PendingRoleClaim, ManagementError> {
    let user_id = validate_governance_user_id(user_id)
        .map_err(|e| ManagementError::InvalidUserId(e.to_string()))?;
    let verify_token = generate_token();
    let claim = store
        .upsert_role_claim(&NewRoleClaim {
            site_id: site_id.to_string(),
            room_id: room_id.to_string(),
            user_id: user_id.clone(),
            level,
            token_hash: token_hash(&verify_token),
            expires_at: Utc::now() + Duration::hours(ROLE_CLAIM_TTL_HOURS),
        })
        .await?;
    Ok(PendingRoleClaim {
        user_id: claim.user_id,
        level,
        verify_token,
        expires_at: claim.expires_at,
    })
}

/// Bootstraps a freshly registered site through the bot's self-service path:
/// registers the sender's own Matrix account as the first site admin and
/// records an applied role claim for audit.
///
/// This is deliberately a bootstrap-only use case. Subsequent admin
/// appointments and revocations remain owner-only operations behind the
/// claim-token API; the bot never receives a "manage admins" command.
pub async fn bootstrap_first_site_admin(
    role_claims: &dyn RoleClaimStore,
    driver: &dyn MatrixDriver,
    site_service: &SiteService,
    site_id: &str,
    user_id: &str,
) -> Result<(), ManagementError> {
    let user_id = validate_governance_user_id(user_id)
        .map_err(|e| ManagementError::InvalidUserId(e.to_string()))?;
    let site_id = SiteId::new(site_id.to_string())
        .map_err(|e| ManagementError::InvalidSiteId(e.to_string()))?;
    let space_id = site_service.ensure_space(&site_id, driver).await?;

    let verify_token = generate_token();
    let claim = role_claims
        .upsert_role_claim(&NewRoleClaim {
            site_id: site_id.as_str().to_string(),
            room_id: String::new(),
            user_id: user_id.clone(),
            level: SITE_ADMIN_LEVEL,
            token_hash: token_hash(&verify_token),
            expires_at: Utc::now() + Duration::hours(ROLE_CLAIM_TTL_HOURS),
        })
        .await?;

    set_role_level(driver, &space_id, &user_id, SITE_ADMIN_LEVEL, true).await?;
    if let Err(error) = driver.invite_user(&space_id, &user_id).await {
        warn!(
            "first admin bootstrap: invite {} to {space_id} failed: {error:#}",
            user_id
        );
    }

    if !role_claims.mark_claim_activated(claim.id).await? {
        return Err(ManagementError::Infra(anyhow::anyhow!(
            "failed to activate first-admin bootstrap claim"
        )));
    }
    role_claims.mark_claim_applied(claim.id).await?;
    Ok(())
}

/// Starts a site ownership handover: creates a pending site-admin claim for
/// the target and records a pending transfer with the same expiry. The
/// claim-token holder (owner) remains in control until the target verifies.
pub async fn start_owner_transfer(
    role_claims: &dyn RoleClaimStore,
    transfers: &dyn SiteTransferStore,
    site_id: &str,
    user_id: &str,
) -> Result<(PendingRoleClaim, SiteTransfer), ManagementError> {
    let pending = create_role_claim(role_claims, site_id, "", user_id, SITE_ADMIN_LEVEL).await?;
    let transfer = transfers
        .upsert_pending_transfer(site_id, &pending.user_id, pending.expires_at)
        .await?;
    Ok((pending, transfer))
}

/// Completes a verified ownership transfer: resets the site-admin roster to
/// the new owner's verified account, rotates the claim token, and delivers
/// the new token through the target's claim DM.
///
/// Returns `false` when no matching pending transfer exists (the caller
/// should leave the claim applied as a plain admin registration).
pub async fn complete_owner_transfer(
    transfers: &dyn SiteTransferStore,
    site_auth: &dyn SiteAuthStore,
    driver: &dyn MatrixDriver,
    site_service: &SiteService,
    site_id: &str,
    target_mxid: &str,
    dm_room_id: &str,
) -> Result<bool, ManagementError> {
    let Some(transfer) = transfers.find_pending_transfer(site_id).await? else {
        return Ok(false);
    };
    if transfer.target_mxid != target_mxid {
        return Ok(false);
    }

    let site_id = SiteId::new(site_id.to_string())
        .map_err(|e| ManagementError::InvalidSiteId(e.to_string()))?;
    let space_id = site_service.ensure_space(&site_id, driver).await?;
    let power_levels = driver
        .get_room_power_levels(&space_id)
        .await?
        .unwrap_or_else(|| serde_json::json!({}));
    let sender = driver.sender_user_id().unwrap_or_default();
    for role in role_entries(&power_levels, SITE_ADMIN_LEVEL) {
        if role.user_id == target_mxid || role.user_id == sender {
            continue;
        }
        set_role_level(driver, &space_id, &role.user_id, SITE_ADMIN_LEVEL, false).await?;
    }

    let new_token = rotate_claim_token(site_auth, site_id.as_str())
        .await?
        .ok_or_else(|| ManagementError::SiteNotApiRegistered(site_id.as_str().to_string()))?;
    if let Err(error) = driver
        .send_bot_message(
            dm_room_id,
            &format!("新 claim token（只显示一次，请勿转发）：\n{new_token}"),
        )
        .await
    {
        warn!(
            "owner transfer: failed to deliver new claim token to {} in {dm_room_id}: {error:#}",
            transfer.target_mxid
        );
    }
    transfers
        .complete_transfer(site_id.as_str(), transfer.id)
        .await?;

    Ok(true)
}

/// Removes a site-level role: cancels a pending claim, or removes an applied
/// role from the Space power levels (the governance pass then propagates the
/// removal into every comment room).
pub async fn remove_site_role(
    store: &dyn RoleClaimStore,
    governance: &dyn GovernanceStore,
    driver: &dyn MatrixDriver,
    site_service: &SiteService,
    site_id: &str,
    user_id: &str,
    level: i64,
) -> Result<RoleRemoval, ManagementError> {
    let user_id = validate_governance_user_id(user_id)
        .map_err(|e| ManagementError::InvalidUserId(e.to_string()))?;
    if store
        .revoke_role_claim(site_id, "", &user_id, level)
        .await?
    {
        return Ok(RoleRemoval::PendingRevoked);
    }

    let applied = governance
        .list_site_roles(site_id)
        .await?
        .iter()
        .any(|role| role.user_id == user_id && role.level == level);
    if !applied {
        return Err(ManagementError::RoleNotFound);
    }

    let site_id = SiteId::new(site_id.to_string())
        .map_err(|e| ManagementError::InvalidSiteId(e.to_string()))?;
    let space_id = site_service.ensure_space(&site_id, driver).await?;
    set_role_level(driver, &space_id, &user_id, level, false).await?;
    store
        .mark_applied_claim_revoked(site_id.as_str(), "", &user_id, level)
        .await?;
    Ok(RoleRemoval::AppliedRemoved)
}

/// Removes an already-applied room moderator from a room's power levels.
/// Site-level roles (>= manager) are rejected so this cannot fight the
/// governance sync pass.
pub async fn remove_room_moderator(
    store: &dyn RoleClaimStore,
    governance: &dyn GovernanceStore,
    driver: &dyn MatrixDriver,
    site_id: &str,
    room_id: &str,
    user_id: &str,
) -> Result<RoleRemoval, ManagementError> {
    let user_id = validate_governance_user_id(user_id)
        .map_err(|e| ManagementError::InvalidUserId(e.to_string()))?;
    if store
        .revoke_role_claim(site_id, room_id, &user_id, MODERATOR_LEVEL)
        .await?
    {
        return Ok(RoleRemoval::PendingRevoked);
    }

    let applied = governance
        .list_room_roles(room_id)
        .await?
        .iter()
        .any(|role| role.user_id == user_id && role.level == MODERATOR_LEVEL);
    if !applied {
        return Err(ManagementError::RoleNotFound);
    }

    let current = driver
        .get_room_power_levels(room_id)
        .await?
        .unwrap_or_else(|| serde_json::json!({}));
    let current_level = role_entries(&current, MODERATOR_LEVEL)
        .into_iter()
        .find(|role| role.user_id == user_id)
        .map(|role| role.level)
        .unwrap_or(0);
    if current_level >= MANAGER_LEVEL {
        return Err(ManagementError::SiteLevelRoleConflict);
    }

    set_role_level(driver, room_id, &user_id, MODERATOR_LEVEL, false).await?;
    store
        .mark_applied_claim_revoked(site_id, room_id, &user_id, MODERATOR_LEVEL)
        .await?;
    Ok(RoleRemoval::AppliedRemoved)
}

/// Rotates a site's claim token, returning the new plaintext token.
/// Returns `None` when the site does not exist.
pub async fn rotate_claim_token(
    store: &dyn SiteAuthStore,
    site_id: &str,
) -> Result<Option<String>, ManagementError> {
    let token = generate_token();
    if !store
        .rotate_claim_token(site_id, &token_hash(&token))
        .await?
    {
        return Ok(None);
    }
    Ok(Some(token))
}

/// Marks a site `retiring`; writes stop immediately and the running server's
/// reconciler performs the Matrix retirement. Returns `false` when the
/// site does not exist or is already retired.
pub async fn retire_site(
    store: &dyn SiteAuthStore,
    site_id: &str,
) -> Result<bool, ManagementError> {
    Ok(store.mark_site_retiring(site_id).await?)
}

/// Marks one page's active comment room `Retired`, stopping new writes
/// immediately; the running reconciler then leaves the Matrix room and
/// clears local projections. Returns `false` when there is no active room
/// for the site/post (or it is already retired), matching `retire_site`.
pub async fn retire_page_room(
    registry: &dyn RegistryStore,
    site_id: &SiteId,
    page_slug: &PageSlug,
) -> Result<bool, ManagementError> {
    let Some(room_id) = registry.get_registered_room(site_id, page_slug).await? else {
        return Ok(false);
    };
    Ok(registry.mark_room_retired(&room_id).await?)
}

/// Room-id variant of [`retire_page_room`], used by the operator mirror and
/// the CLI/bot room commands.
pub async fn retire_page_room_by_room_id(
    registry: &dyn RegistryStore,
    room_id: &str,
) -> Result<bool, ManagementError> {
    let Some(identity) = registry.get_registered_room_identity(room_id).await? else {
        return Ok(false);
    };
    let site_id = SiteId::new(identity.site_id.clone())
        .map_err(|_| ManagementError::InvalidSiteId(identity.site_id.clone()))?;
    let page_slug = PageSlug::new(identity.page_slug.clone())
        .map_err(|_| ManagementError::InvalidPageSlug(identity.page_slug.clone()))?;
    retire_page_room(registry, &site_id, &page_slug).await
}

/// Issues a fresh HMAC secret for a site. Returns `None` when the site does
/// not exist.
pub async fn issue_secret(
    store: &dyn SiteAuthStore,
    site_id: &str,
) -> Result<Option<String>, ManagementError> {
    if store.get_site_auth(site_id).await?.is_none() {
        return Ok(None);
    }
    let secret = generate_token();
    store.store_site_secret(site_id, &secret).await?;
    Ok(Some(secret))
}

/// One site in the effective list: database-tracked or operator-declared in
/// the `[sites]` configuration overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSite {
    pub site_id: String,
    pub from_config: bool,
}

/// Enumerates the effective sites: database rows merged with config-only
/// overlay entries, sorted by site id. This is the single source of truth
/// for "which sites exist" used by CLI, API and the bot.
pub async fn list_effective_sites(
    store: &dyn SiteAuthStore,
    policy: &SiteAuthPolicy,
) -> Result<Vec<EffectiveSite>, ManagementError> {
    let mut sites: Vec<EffectiveSite> = store
        .list_site_auth()
        .await?
        .into_iter()
        .map(|site| EffectiveSite {
            site_id: site.site_id,
            from_config: false,
        })
        .collect();
    let known: HashSet<String> = sites.iter().map(|site| site.site_id.clone()).collect();
    for site_id in policy.sites.keys() {
        if !known.contains(site_id) {
            sites.push(EffectiveSite {
                site_id: site_id.clone(),
                from_config: true,
            });
        }
    }
    sites.sort_by(|a, b| a.site_id.cmp(&b.site_id));
    Ok(sites)
}
