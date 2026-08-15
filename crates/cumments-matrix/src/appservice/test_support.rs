//! Shared test doubles and mock-homeserver helpers for driver unit tests.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use cumments_core::{models::SiteId, ports::VirtualUserStore};
use serde_json::json;
use std::sync::Arc;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub(crate) const ROOM_ID: &str = "!room:example.com";
pub(crate) const CREATE_EVENTS_PATH: &str =
    "/_matrix/client/v3/rooms/%21room%3Aexample.com/messages";
pub(crate) const MEMBERSHIP_PATH: &str = "/_matrix/client/v3/rooms/%21room%3Aexample.com/state/m.room.member/%40_cumments_bot%3Aexample.com";
pub(crate) const POWER_LEVELS_PATH: &str =
    "/_matrix/client/v3/rooms/%21room%3Aexample.com/state/m.room.power_levels";
pub(crate) const CREATE_STATE_PATH: &str =
    "/_matrix/client/v3/rooms/%21room%3Aexample.com/state/m.room.create";
pub(crate) const TOMBSTONE_STATE_PATH: &str =
    "/_matrix/client/v3/rooms/%21room%3Aexample.com/state/m.room.tombstone";
pub(crate) const UPGRADE_PATH: &str = "/_matrix/client/v3/rooms/%21room%3Aexample.com/upgrade";

pub(crate) struct StubVirtualUserStore;

#[async_trait]
impl VirtualUserStore for StubVirtualUserStore {
    async fn get_or_create_virtual_user(
        &self,
        author_public_key: &str,
        site_id: &SiteId,
        server_name: &str,
    ) -> Result<String> {
        Ok(format!(
            "@_cumments_{}_{}:{}",
            site_id.as_str(),
            author_public_key,
            server_name
        ))
    }

    async fn list_virtual_users_for_site(&self, _site_id: &SiteId) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}

pub(crate) fn test_driver(server: &MockServer) -> AppServiceMatrixDriver {
    AppServiceMatrixDriver::new(
        server.uri(),
        "test-token".to_string(),
        "example.com".to_string(),
        "_cumments_bot".to_string(),
        Arc::new(StubVirtualUserStore),
        None,
    )
    .expect("build test driver")
}

pub(crate) async fn mount_create_events(server: &MockServer, first_event: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(CREATE_EVENTS_PATH))
        .and(query_param("dir", "f"))
        .and(query_param("limit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "start": "s",
            "end": "e",
            "chunk": [first_event]
        })))
        .expect(1)
        .mount(server)
        .await;
}

pub(crate) async fn mount_joined_membership(server: &MockServer, expected: u64) {
    Mock::given(method("GET"))
        .and(path(MEMBERSHIP_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "membership": "join"
        })))
        .expect(expected)
        .mount(server)
        .await;
}

pub(crate) async fn mount_power_levels(
    server: &MockServer,
    content: serde_json::Value,
    expected: u64,
) {
    Mock::given(method("GET"))
        .and(path(POWER_LEVELS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(content))
        .expect(expected)
        .mount(server)
        .await;
}
