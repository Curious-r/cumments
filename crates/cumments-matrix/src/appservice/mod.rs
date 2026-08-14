//! AppService MatrixDriver – uses `reqwest` to call the Matrix CS API
//! directly with the AppService `as_token`, supporting virtual users.

mod membership;
mod messages;
mod rooms;
#[cfg(test)]
mod test_support;
mod trait_impl;
mod versions;

use anyhow::{Result, anyhow};
use cumments_core::ports::VirtualUserStore;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Build a typed adoption-refusal error so the reconciler can quarantine the
/// room without matching error strings.
pub(crate) fn adoption_refused(room_id: &str, reason: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(cumments_core::matrix_error::MatrixError::AdoptionRefused {
        room_id: room_id.to_string(),
        reason: reason.into(),
    })
}

/// Build a typed room-gone error so the reconciler can retire the registry
/// entry without matching error strings.
pub(crate) fn room_gone(room_id: &str, reason: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(cumments_core::matrix_error::MatrixError::RoomGone {
        room_id: room_id.to_string(),
        reason: reason.into(),
    })
}

/// The AppService-based Matrix driver.
///
/// This driver authenticates with the AppService `as_token` and can
/// impersonate any virtual user in the AppService namespace.
pub struct AppServiceMatrixDriver {
    http_client: reqwest::Client,
    homeserver_url: String,
    as_token: String,
    server_name: String,
    sender_localpart: String,
    virtual_user_store: Arc<dyn VirtualUserStore>,
    joined_cache: Mutex<HashSet<(String, String)>>,
    display_name_cache: Mutex<HashMap<String, String>>,
    /// Explicit room version from configuration, if any.
    room_version_override: Option<String>,
    /// Cached `m.room_versions.default` from `/capabilities`.
    default_room_version: Mutex<Option<String>>,
    /// Set once when `/capabilities` is unreachable or unusable for this
    /// homeserver (e.g. appservice credentials are rejected). Room-version
    /// preflight checks then skip further queries and warn only once per
    /// process, letting `createRoom` decide.
    capabilities_unavailable: Mutex<bool>,
}

impl AppServiceMatrixDriver {
    pub fn new(
        homeserver_url: String,
        as_token: String,
        server_name: String,
        sender_localpart: String,
        virtual_user_store: Arc<dyn VirtualUserStore>,
        room_version: Option<String>,
    ) -> Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow!("failed to build HTTP client: {e}"))?;
        Ok(Self {
            http_client,
            homeserver_url,
            as_token,
            server_name,
            sender_localpart,
            virtual_user_store,
            joined_cache: Mutex::new(HashSet::new()),
            display_name_cache: Mutex::new(HashMap::new()),
            room_version_override: room_version,
            default_room_version: Mutex::new(None),
            capabilities_unavailable: Mutex::new(false),
        })
    }

    // ── Internal helpers ──────────────────────────────────────────

    /// Build a fully qualified user ID from a localpart.
    fn user_id(&self, localpart: &str) -> String {
        format!("@{}:{}", localpart, self.server_name)
    }

    /// The AppService sender user ID used for room creation, state events
    /// and redactions.
    fn sender_user_id(&self) -> String {
        self.user_id(&self.sender_localpart)
    }

    /// Make an authenticated CS API request with optional virtual user.
    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        virtual_user: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let url = format!("{}/{}", self.homeserver_url.trim_end_matches('/'), path);
        let mut req = self
            .http_client
            .request(method, &url)
            .header("Authorization", format!("Bearer {}", self.as_token));
        if let Some(vu) = virtual_user {
            req = req.query(&[("user_id", vu)]);
        }
        req
    }
}
