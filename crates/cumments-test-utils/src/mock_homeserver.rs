//! Mock Matrix homeserver backed by `wiremock::MockServer`.
//!
//! Provides a real HTTP-level MockHomeserver to test Matrix client drivers,
//! room context resolution, profile inspection, and error handling over
//! actual network requests.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use cumments_core::models::SiteId;
use cumments_core::ports::{HistoricalRoomStateResolver, VirtualUserStore};
use cumments_matrix::{AppServiceMatrixDriver, percent_encode};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Stub virtual user store for test drivers.
pub struct StubVirtualUserStore;

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

/// Mock Matrix homeserver running an in-process HTTP server.
pub struct MockHomeserver {
    server: MockServer,
}

impl MockHomeserver {
    /// Starts a new mock homeserver on a random local port.
    pub async fn start() -> Self {
        Self {
            server: MockServer::start().await,
        }
    }

    /// The base URI of the mock homeserver (e.g. `http://127.0.0.1:12345`).
    pub fn uri(&self) -> String {
        self.server.uri()
    }

    /// Access the underlying `wiremock::MockServer`.
    pub fn server(&self) -> &MockServer {
        &self.server
    }

    /// Builds a real `AppServiceMatrixDriver` targeting this mock homeserver.
    pub fn driver(&self) -> Arc<AppServiceMatrixDriver> {
        Arc::new(
            AppServiceMatrixDriver::new(
                self.server.uri(),
                "test-as-token".to_string(),
                "hs".to_string(),
                "_cumments_bot".to_string(),
                Arc::new(StubVirtualUserStore),
                None,
            )
            .expect("build AppServiceMatrixDriver for MockHomeserver"),
        )
    }

    /// Convenience to obtain an `Arc<dyn HistoricalRoomStateResolver>` backed by this homeserver.
    pub fn historical_resolver(&self) -> Arc<dyn HistoricalRoomStateResolver> {
        self.driver() as Arc<dyn HistoricalRoomStateResolver>
    }

    /// Mounts a `GET /_matrix/client/v3/rooms/{roomId}/context/{eventId}?limit=0` endpoint
    /// returning HTTP 200 with the provided `event` and `state`.
    pub async fn mount_context(
        &self,
        room_id: &str,
        event_id: &str,
        event: serde_json::Value,
        state: Vec<serde_json::Value>,
    ) {
        let expected_path = format!(
            "/_matrix/client/v3/rooms/{}/context/{}",
            percent_encode(room_id),
            percent_encode(event_id)
        );
        Mock::given(method("GET"))
            .and(path(expected_path))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "event": event,
                "state": state,
            })))
            .mount(&self.server)
            .await;
    }

    /// Mounts a context response with a specific status code and JSON body.
    pub async fn mount_context_response(
        &self,
        room_id: &str,
        event_id: &str,
        status: u16,
        body: serde_json::Value,
    ) {
        let expected_path = format!(
            "/_matrix/client/v3/rooms/{}/context/{}",
            percent_encode(room_id),
            percent_encode(event_id)
        );
        Mock::given(method("GET"))
            .and(path(expected_path))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&self.server)
            .await;
    }

    /// Mounts a context response with a specific status code and raw string body.
    pub async fn mount_context_raw(
        &self,
        room_id: &str,
        event_id: &str,
        status: u16,
        body_str: &str,
    ) {
        let expected_path = format!(
            "/_matrix/client/v3/rooms/{}/context/{}",
            percent_encode(room_id),
            percent_encode(event_id)
        );
        Mock::given(method("GET"))
            .and(path(expected_path))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body_str))
            .mount(&self.server)
            .await;
    }

    /// Helper to build a canonical Matrix `m.room.member` state event JSON value.
    pub fn make_member_state_event(
        user_id: &str,
        membership: &str,
        displayname: Option<&str>,
        avatar_url: Option<&str>,
    ) -> serde_json::Value {
        let mut content = json!({
            "membership": membership,
        });
        if let Some(dn) = displayname {
            content["displayname"] = json!(dn);
        }
        if let Some(av) = avatar_url {
            content["avatar_url"] = json!(av);
        }
        json!({
            "type": "m.room.member",
            "state_key": user_id,
            "sender": user_id,
            "content": content,
        })
    }

    /// Helper to build a canonical Matrix `m.room.message` event JSON value.
    pub fn make_message_event(
        room_id: &str,
        event_id: &str,
        sender: &str,
        body: &str,
        ts: i64,
    ) -> serde_json::Value {
        json!({
            "type": "m.room.message",
            "room_id": room_id,
            "event_id": event_id,
            "sender": sender,
            "origin_server_ts": ts,
            "content": {
                "msgtype": "m.text",
                "body": body,
            }
        })
    }
}
