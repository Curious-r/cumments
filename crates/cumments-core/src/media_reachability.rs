//! Media reachability evaluator for Cumments-managed media lifecycles.
//!
//! Evaluates whether a media candidate is logically reachable across:
//! 1. Current global visitor profile (via Matrix homeserver driver)
//! 2. Historical author presentations (via retained message snapshots)
//! 3. Content attachments (via retained comments, revisions, stickers, and active submissions)
//!
//! Ownership (`CummentsOwned` vs `NotOwned`) is evaluated independently from
//! reachability (`Reachable`, `Unreachable`, `Unknown`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::models::SiteId;
use crate::ports::{MatrixDriver, MessageStore};

/// Logical reachability state of a media resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReachabilityState {
    /// Authoritatively referenced by at least one semantic source.
    Reachable,
    /// Checked across all semantic sources and found unreferenced.
    Unreachable,
    /// Could not be deterministically evaluated (e.g. I/O or query failure, missing mapping).
    Unknown,
}

impl ReachabilityState {
    pub fn is_reachable(&self) -> bool {
        matches!(self, Self::Reachable)
    }

    pub fn is_unreachable(&self) -> bool {
        matches!(self, Self::Unreachable)
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

/// Provenance and durable ownership of a media resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaOwnership {
    /// Backed by a durable local `media_uploads` record for the site.
    CummentsOwned,
    /// Not an upload owned by Cumments (e.g. externally discovered avatar).
    NotOwned,
}

impl MediaOwnership {
    pub fn is_cumments_owned(&self) -> bool {
        matches!(self, Self::CummentsOwned)
    }
}

/// Evaluation of reachability across independent reference domains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaReachability {
    pub current_profile: ReachabilityState,
    pub historical_presentation: ReachabilityState,
    pub content_attachment: ReachabilityState,
    pub overall: ReachabilityState,
}

impl MediaReachability {
    pub fn new(
        current_profile: ReachabilityState,
        historical_presentation: ReachabilityState,
        content_attachment: ReachabilityState,
    ) -> Self {
        let overall = if current_profile == ReachabilityState::Reachable
            || historical_presentation == ReachabilityState::Reachable
            || content_attachment == ReachabilityState::Reachable
        {
            ReachabilityState::Reachable
        } else if current_profile == ReachabilityState::Unknown
            || historical_presentation == ReachabilityState::Unknown
            || content_attachment == ReachabilityState::Unknown
        {
            ReachabilityState::Unknown
        } else {
            ReachabilityState::Unreachable
        };

        Self {
            current_profile,
            historical_presentation,
            content_attachment,
            overall,
        }
    }

    pub fn unknown() -> Self {
        Self {
            current_profile: ReachabilityState::Unknown,
            historical_presentation: ReachabilityState::Unknown,
            content_attachment: ReachabilityState::Unknown,
            overall: ReachabilityState::Unknown,
        }
    }
}

/// Durable local upload metadata recorded in `media_uploads`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaUploadRecord {
    pub id: i64,
    pub mxc_url: String,
    pub author_public_key: String,
    pub site_id: String,
    pub page_slug: Option<String>,
    pub used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub submission_id: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Result of evaluating a media candidate's ownership and reachability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaEvaluationResult {
    pub candidate_mxc: String,
    pub site_id: SiteId,
    pub ownership: MediaOwnership,
    pub reachability: MediaReachability,
}

impl MediaEvaluationResult {
    /// Pure calculation determining whether this candidate is eligible for cleanup:
    /// must be Cumments-owned AND overall reachability must be `Unreachable`.
    pub fn is_cleanup_eligible(&self) -> bool {
        self.ownership == MediaOwnership::CummentsOwned
            && self.reachability.overall == ReachabilityState::Unreachable
    }
}

/// Evaluator service that determines the logical reachability and ownership
/// of Matrix media resources without mutating durable state.
pub struct MediaReachabilityEvaluator {
    driver: Arc<dyn MatrixDriver>,
    message_store: Arc<dyn MessageStore>,
}

impl MediaReachabilityEvaluator {
    pub fn new(driver: Arc<dyn MatrixDriver>, message_store: Arc<dyn MessageStore>) -> Self {
        Self {
            driver,
            message_store,
        }
    }

    /// Evaluates a single candidate MXC for the specified site.
    pub async fn evaluate_candidate(
        &self,
        site_id: &SiteId,
        mxc_url: &str,
    ) -> MediaEvaluationResult {
        // 1. Check Cumments ownership
        let upload_record = match self
            .message_store
            .get_media_upload(site_id.as_str(), mxc_url)
            .await
        {
            Ok(record) => record,
            Err(err) => {
                tracing::warn!(%err, site_id = %site_id.as_str(), mxc = %mxc_url, "failed to check media upload ownership");
                return MediaEvaluationResult {
                    candidate_mxc: mxc_url.to_string(),
                    site_id: site_id.clone(),
                    ownership: MediaOwnership::NotOwned,
                    reachability: MediaReachability::unknown(),
                };
            }
        };

        let (ownership, author_public_key) = match upload_record {
            Some(upload) => (
                MediaOwnership::CummentsOwned,
                Some(upload.author_public_key),
            ),
            None => (MediaOwnership::NotOwned, None),
        };

        // 2. Source A: Current Global Profile
        let current_profile = match author_public_key {
            Some(ref pubkey) => match self.driver.get_profile(pubkey, site_id).await {
                Ok(Some(profile)) => {
                    if profile.avatar_url.as_deref() == Some(mxc_url) {
                        ReachabilityState::Reachable
                    } else {
                        ReachabilityState::Unreachable
                    }
                }
                Ok(None) => ReachabilityState::Unreachable,
                Err(err) => {
                    tracing::warn!(%err, site_id = %site_id.as_str(), author = %pubkey, "failed to query visitor Matrix profile");
                    ReachabilityState::Unknown
                }
            },
            None => ReachabilityState::Unreachable,
        };

        // 3. Source B: Historical Author Presentation
        let historical_presentation = match self
            .message_store
            .has_historical_author_avatar(site_id.as_str(), mxc_url)
            .await
        {
            Ok(true) => ReachabilityState::Reachable,
            Ok(false) => ReachabilityState::Unreachable,
            Err(err) => {
                tracing::warn!(%err, site_id = %site_id.as_str(), mxc = %mxc_url, "failed to query historical author avatars");
                ReachabilityState::Unknown
            }
        };

        // 4. Source C: Content Attachments
        let content_attachment = match self
            .message_store
            .has_content_attachment(site_id.as_str(), mxc_url)
            .await
        {
            Ok(true) => ReachabilityState::Reachable,
            Ok(false) => ReachabilityState::Unreachable,
            Err(err) => {
                tracing::warn!(%err, site_id = %site_id.as_str(), mxc = %mxc_url, "failed to query content attachments");
                ReachabilityState::Unknown
            }
        };

        let reachability =
            MediaReachability::new(current_profile, historical_presentation, content_attachment);

        MediaEvaluationResult {
            candidate_mxc: mxc_url.to_string(),
            site_id: site_id.clone(),
            ownership,
            reachability,
        }
    }

    /// Evaluates all Cumments-owned upload candidates for the given site.
    pub async fn evaluate_site_owned_candidates(
        &self,
        site_id: &SiteId,
    ) -> anyhow::Result<Vec<MediaEvaluationResult>> {
        let uploads = self
            .message_store
            .list_media_uploads_for_site(site_id.as_str())
            .await?;
        let mut results = Vec::with_capacity(uploads.len());
        for upload in uploads {
            results.push(self.evaluate_candidate(site_id, &upload.mxc_url).await);
        }
        Ok(results)
    }
}
