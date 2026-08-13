//! Room-version discovery and preflight checks against `/capabilities`.

use super::*;
use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use tracing::warn;

#[derive(Deserialize)]
struct CapabilitiesResponse {
    room_versions: Option<RoomVersions>,
}

#[derive(Deserialize)]
struct RoomVersions {
    default: String,
    #[serde(default)]
    available: HashMap<String, String>,
}

impl AppServiceMatrixDriver {
    /// `/capabilities` (cached after the first lookup). `None` means unknown;
    /// callers then assume the pre-v12 behaviour.
    pub(super) async fn effective_room_version(&self) -> Option<String> {
        if let Some(version) = &self.room_version_override {
            return Some(version.clone());
        }
        if let Some(version) = self
            .default_room_version
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Some(version);
        }
        if self.capabilities_unavailable() {
            return None;
        }

        let resp = match self
            .request(reqwest::Method::GET, "_matrix/client/v3/capabilities", None)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                self.note_capabilities_unavailable(&format!("query failed: {e:#}"));
                return None;
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            self.note_capabilities_unavailable(&format!("{status}: {error_body}"));
            return None;
        }

        let caps = match resp.json::<CapabilitiesResponse>().await {
            Ok(caps) => caps,
            Err(e) => {
                self.note_capabilities_unavailable(&format!("parse failed: {e:#}"));
                return None;
            }
        };
        let Some(version) = caps
            .room_versions
            .and_then(|r| (!r.default.is_empty()).then_some(r.default))
        else {
            self.note_capabilities_unavailable("no default room version advertised");
            return None;
        };
        *self
            .default_room_version
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(version.clone());
        Some(version)
    }

    /// Best-effort pre-check that the configured `room_version` is supported
    /// by the homeserver before creating a new room.
    ///
    /// The check only fails on a *definitive* answer: a successful
    /// `/capabilities` response that lists room versions without the
    /// configured one. Homeservers that reject appservice credentials (401)
    /// or do not advertise room versions are treated as unknown — the
    /// authoritative decision is left to `createRoom`, whose error surfaces
    /// in the submission.s `last_error`.
    pub(super) async fn validate_room_version_override(&self) -> Result<()> {
        let Some(version) = &self.room_version_override else {
            return Ok(());
        };
        if self.capabilities_unavailable() {
            return Ok(());
        }
        let resp = self
            .request(reqwest::Method::GET, "_matrix/client/v3/capabilities", None)
            .send()
            .await;
        let resp = match resp {
            Ok(resp) => resp,
            Err(e) => {
                self.note_capabilities_unavailable(&format!("query failed: {e}"));
                return Ok(());
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            self.note_capabilities_unavailable(&format!("{status}: {error_body}"));
            return Ok(());
        }
        let caps: CapabilitiesResponse = match resp.json().await {
            Ok(caps) => caps,
            Err(e) => {
                self.note_capabilities_unavailable(&format!("parse failed: {e}"));
                return Ok(());
            }
        };
        let available = caps.room_versions.map(|r| r.available).unwrap_or_default();
        if available.is_empty() {
            self.note_capabilities_unavailable("no room versions advertised");
            return Ok(());
        }
        room_version_decision(version, &available).map_err(anyhow::Error::msg)
    }

    /// Whether `/capabilities` has already been marked unusable for this
    /// process.
    fn capabilities_unavailable(&self) -> bool {
        *self
            .capabilities_unavailable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Marks `/capabilities` unusable for this process and warns exactly once.
    fn note_capabilities_unavailable(&self, reason: &str) {
        let mut guard = self
            .capabilities_unavailable
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *guard {
            return;
        }
        *guard = true;
        warn!(
            "Homeserver capabilities are unavailable for this process ({reason}); \
             skipping room-version checks and letting createRoom decide until restart"
        );
    }
}

/// Pure decision for a `/capabilities` room-version list.
///
/// An empty list means the homeserver did not advertise room versions: the
/// version can neither be confirmed nor ruled out, so the caller proceeds and
/// lets `createRoom` decide. A non-empty list is definitive — the version
/// either is supported or the configuration is invalid.
fn room_version_decision(version: &str, available: &HashMap<String, String>) -> Result<(), String> {
    if available.is_empty() {
        return Ok(());
    }
    if available.contains_key(version) {
        Ok(())
    } else {
        Err(format!(
            "configured room_version `{version}` is not supported by homeserver /capabilities \
             (available: {})",
            available.keys().cloned().collect::<Vec<_>>().join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capabilities_response_parses_default_room_version() {
        let caps: CapabilitiesResponse = serde_json::from_value(json!({
            "room_versions": {
                "default": "12",
                "available": { "11": "stable", "12": "stable" }
            }
        }))
        .expect("capabilities parse");
        assert_eq!(
            caps.room_versions.map(|r| r.default),
            Some("12".to_string())
        );

        let no_versions: CapabilitiesResponse =
            serde_json::from_value(json!({})).expect("empty capabilities parse");
        assert!(no_versions.room_versions.is_none());
    }

    #[test]
    fn room_version_decision_accepts_supported_and_empty_lists() {
        let mut available = HashMap::new();
        available.insert("12".to_string(), "stable".to_string());
        assert!(room_version_decision("12", &available).is_ok());
        assert!(
            room_version_decision("11", &available).is_err(),
            "a definitive list without the version must fail"
        );

        let empty = HashMap::new();
        assert!(
            room_version_decision("12", &empty).is_ok(),
            "no advertised versions must be treated as unknown, not unsupported"
        );
    }

    #[test]
    fn room_version_decision_error_lists_available_versions() {
        let mut available = HashMap::new();
        available.insert("10".to_string(), "stable".to_string());
        available.insert("11".to_string(), "stable".to_string());
        let error = room_version_decision("12", &available).expect_err("must reject");
        assert!(error.contains("`12` is not supported"));
        assert!(
            error.contains("10") && error.contains("11"),
            "error must list available versions: {error}"
        );
    }
}
