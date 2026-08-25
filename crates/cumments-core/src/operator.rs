//! Operator-facing views and listing use cases, shared by the Operator API,
//! the CLI and the bot. DTOs live here so every adapter renders the same
//! shape and the `[sites]` overlay merge is implemented exactly once.

use crate::models::{PaginationMeta, QuarantinedRoom};
use crate::ports::{RegistryStore, SiteAuthStore};
use crate::site_auth::{
    SiteAuthInfo, SiteAuthMode, SiteAuthPolicy, SiteLifecycle, SitePolicyEntry,
    SiteVerificationStatus,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A page of operator-facing records.
#[derive(Debug, Serialize)]
pub struct OperatorPage<T> {
    pub data: Vec<T>,
    pub meta: PaginationMeta,
}

/// One operator-visible site (database row merged with the config overlay).
#[derive(Debug, Serialize)]
pub struct OperatorSite {
    pub site_id: String,
    pub lifecycle: SiteLifecycle,
    pub auth_mode: SiteAuthMode,
    pub verification_status: SiteVerificationStatus,
    pub origins: Vec<OperatorOrigin>,
    pub verified_at: Option<DateTime<Utc>>,
    pub has_secret: bool,
    pub has_claim_token: bool,
    pub updated_at: Option<DateTime<Utc>>,
}

/// One allowed origin on a site, annotated with its source.
#[derive(Debug, Serialize)]
pub struct OperatorOrigin {
    pub origin: String,
    /// `config` (operator-declared) or `verified` (self-service proof).
    pub source: &'static str,
}

/// Query parameters for operator list endpoints.
#[derive(Debug, Deserialize)]
pub struct OperatorListQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub site_id: Option<String>,
}

/// Resolves pagination bounds with sane defaults.
pub fn operator_page_bounds(query: &OperatorListQuery) -> (i64, i64) {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    (page, per_page)
}

/// Builds pagination metadata shared by operator and comment listings.
pub fn operator_meta(total: i64, page: i64, per_page: i64) -> PaginationMeta {
    let total_pages = if total > 0 {
        (total + per_page - 1) / per_page
    } else {
        0
    };
    PaginationMeta {
        total,
        page,
        per_page,
        total_pages,
    }
}

/// Operator-facing view of one durable native-upgrade intent.
#[derive(Debug, Serialize)]
pub struct OperatorRoomUpgradeIntent {
    pub old_room_id: String,
    pub expected_new_version: String,
    pub replacement_room_id: Option<String>,
    pub status: &'static str,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Query parameters for upgrade-intent listings.
#[derive(Debug, Deserialize)]
pub struct UpgradeIntentListQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub status: Option<String>,
}

/// Validates pagination and narrows the optional review status.
pub fn upgrade_intent_query_bounds(
    query: &UpgradeIntentListQuery,
) -> anyhow::Result<(i64, i64, Option<String>)> {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let status = match query.status.as_deref().filter(|s| !s.is_empty()) {
        Some("requested" | "observed" | "adopted" | "failed" | "manual") => query.status.clone(),
        Some(status) => anyhow::bail!("invalid upgrade intent status `{status}`"),
        None => None,
    };
    Ok((page, per_page, status))
}

/// Lists native-upgrade intents so operators can identify failed/manual work.
pub async fn list_operator_upgrade_intents(
    store: &dyn RegistryStore,
    query: &UpgradeIntentListQuery,
) -> Result<OperatorPage<OperatorRoomUpgradeIntent>> {
    let (page, per_page, status_filter) = upgrade_intent_query_bounds(query)?;
    let mut intents = store
        .list_upgrade_intents()
        .await?
        .into_iter()
        .map(|intent| OperatorRoomUpgradeIntent {
            old_room_id: intent.old_room_id,
            expected_new_version: intent.expected_new_version,
            replacement_room_id: intent.replacement_room_id,
            status: intent.status.as_str(),
            error_message: intent.error_message,
            created_at: intent.created_at,
            updated_at: intent.updated_at,
        })
        .collect::<Vec<_>>();
    if let Some(status) = status_filter {
        intents.retain(|intent| intent.status == status);
    }
    let total = intents.len() as i64;
    let start = ((page - 1) * per_page) as usize;
    Ok(OperatorPage {
        data: intents
            .into_iter()
            .skip(start)
            .take(per_page as usize)
            .collect(),
        meta: operator_meta(total, page, per_page),
    })
}

/// Lists managed sites: database rows merged with the `[sites]` overlay.
/// This is the single implementation shared by the API, CLI and bot.
pub async fn list_operator_sites(
    store: &dyn SiteAuthStore,
    policy: &SiteAuthPolicy,
    query: &OperatorListQuery,
) -> Result<OperatorPage<OperatorSite>> {
    let effective = crate::management::list_effective_sites(store, policy).await?;
    let mut sites = Vec::with_capacity(effective.len());
    for site in effective {
        if site.from_config {
            let config = policy.entry(&site.site_id).expect("effective config site");
            sites.push(operator_site_from_config(&site.site_id, config));
        } else {
            let info = store
                .get_site_auth(&site.site_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("site not found"))?;
            sites.push(operator_site(&info, policy.entry(&site.site_id)));
        }
    }
    if let Some(site_id) = query.site_id.as_deref().filter(|s| !s.is_empty()) {
        sites.retain(|site| site.site_id == site_id);
    }
    let (page, per_page) = operator_page_bounds(query);
    let total = sites.len() as i64;
    let start = ((page - 1) * per_page) as usize;
    Ok(OperatorPage {
        data: sites
            .into_iter()
            .skip(start)
            .take(per_page as usize)
            .collect(),
        meta: operator_meta(total, page, per_page),
    })
}

/// Lists quarantined rooms with the same pagination contract as sites.
pub async fn list_operator_quarantined_rooms(
    store: &dyn crate::ports::RegistryStore,
    query: &OperatorListQuery,
) -> Result<OperatorPage<QuarantinedRoom>> {
    let mut rooms = store.get_quarantined_rooms().await?;
    rooms.sort_by(|a, b| a.site_id.cmp(&b.site_id).then(a.room_id.cmp(&b.room_id)));
    if let Some(site_id) = query.site_id.as_deref().filter(|s| !s.is_empty()) {
        rooms.retain(|room| room.site_id == site_id);
    }
    let (page, per_page) = operator_page_bounds(query);
    let total = rooms.len() as i64;
    let start = ((page - 1) * per_page) as usize;
    Ok(OperatorPage {
        data: rooms
            .into_iter()
            .skip(start)
            .take(per_page as usize)
            .collect(),
        meta: operator_meta(total, page, per_page),
    })
}

/// Renders one database-tracked site, merged with its config overlay entry.
pub fn operator_site(info: &SiteAuthInfo, config: Option<&SitePolicyEntry>) -> OperatorSite {
    let mut origins = info
        .verified_origins
        .iter()
        .map(|origin| OperatorOrigin {
            origin: origin.as_str().to_string(),
            source: "verified",
        })
        .collect::<Vec<_>>();
    if let Some(entry) = config {
        origins.extend(entry.allowed_origins.iter().map(|pattern| OperatorOrigin {
            origin: pattern.as_pattern_string(),
            source: "config",
        }));
    }
    origins.sort_by(|a, b| a.origin.cmp(&b.origin));
    origins.dedup_by(|a, b| a.origin == b.origin);

    OperatorSite {
        site_id: info.site_id.clone(),
        lifecycle: info.lifecycle,
        auth_mode: config
            .and_then(|entry| entry.auth_mode)
            .unwrap_or(info.auth_mode),
        verification_status: if config.is_some() {
            SiteVerificationStatus::Verified
        } else {
            info.verification_status
        },
        origins,
        verified_at: info.verified_at,
        has_secret: config.is_some_and(|entry| entry.secret.is_some()) || info.secret.is_some(),
        has_claim_token: info.claim_token_hash.is_some(),
        updated_at: info.updated_at,
    }
}

/// Renders a config-only site (declared in `[sites]`, no database row).
pub fn operator_site_from_config(site_id: &str, entry: &SitePolicyEntry) -> OperatorSite {
    OperatorSite {
        site_id: site_id.to_string(),
        lifecycle: SiteLifecycle::Active,
        auth_mode: entry.auth_mode.unwrap_or(SiteAuthMode::Origin),
        verification_status: SiteVerificationStatus::Verified,
        origins: entry
            .allowed_origins
            .iter()
            .map(|pattern| OperatorOrigin {
                origin: pattern.as_pattern_string(),
                source: "config",
            })
            .collect(),
        verified_at: None,
        has_secret: entry.secret.is_some(),
        has_claim_token: false,
        updated_at: None,
    }
}

/// Builds the TOML block for adopting a database-tracked site into `[sites]`.
pub fn config_snippet_toml(
    site_id: &str,
    db_info: &SiteAuthInfo,
    config_entry: Option<&SitePolicyEntry>,
) -> String {
    let mut origins = db_info
        .verified_origins
        .iter()
        .map(|origin| format!("\"{}\"", origin.as_str()))
        .collect::<Vec<_>>();
    if let Some(entry) = config_entry {
        origins.extend(
            entry
                .allowed_origins
                .iter()
                .map(|pattern| format!("\"{}\"", pattern.as_pattern_string())),
        );
    }
    origins.sort();
    origins.dedup();

    let auth_mode = config_entry
        .and_then(|entry| entry.auth_mode)
        .unwrap_or(db_info.auth_mode);
    let mut toml = format!("[sites.\"{}\"]\n", site_id);
    toml.push_str(&format!("auth_mode = \"{}\"\n", auth_mode.as_str()));
    if !origins.is_empty() {
        toml.push_str(&format!("allowed_origins = [{}]\n", origins.join(", ")));
    }
    if auth_mode == SiteAuthMode::Secret {
        toml.push_str(&format!(
            "# Set the secret via environment instead of this file:\n\
             # CUMMENTS__SITES__{}__SECRET=...\n",
            site_id
        ));
    }
    toml
}
