//! Management use cases shared by every adapter (CLI, API, bot).
//!
//! Adapters own authentication/authorization and response formatting; each
//! operation is implemented exactly once here on top of the core ports.

use crate::governance::{
    CO_MANAGER_LEVEL, MODERATOR_LEVEL, NewRoleClaim, role_entries, set_role_level,
    validate_governance_user_id,
};
use crate::models::SiteId;
use crate::ports::{GovernanceStore, MatrixDriver, RoleClaimStore, SiteAuthStore};
use crate::site_auth::{SiteAuthPolicy, generate_token, token_hash};
use crate::site_service::SiteService;
use chrono::{Duration, Utc};
use std::collections::HashSet;

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
        "this user holds a site-level role; manage them through the owners or co-managers endpoint"
    )]
    SiteLevelRoleConflict,
    #[error(transparent)]
    Infra(#[from] anyhow::Error),
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

/// Removes a site-level role: cancels a pending claim, or removes an applied
/// role from the Space power levels (the moderation pass then propagates the
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
/// Site-level roles (>= co-manager) are rejected so this cannot fight the
/// moderation sync pass.
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
    if current_level >= CO_MANAGER_LEVEL {
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
/// reconciler performs the Matrix decommission. Returns `false` when the
/// site does not exist or is already retired.
pub async fn retire_site(
    store: &dyn SiteAuthStore,
    site_id: &str,
) -> Result<bool, ManagementError> {
    Ok(store.mark_site_retiring(site_id).await?)
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
