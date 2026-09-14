use anyhow::{Result, anyhow};
use cumments_core::models::MemberPresentation;
use serde::Deserialize;
use tracing::instrument;

use crate::appservice::AppServiceMatrixDriver;
use crate::wire::percent_encode;

#[derive(Deserialize)]
struct ContextEventResponse {
    #[serde(rename = "type")]
    event_type: String,
    state_key: Option<String>,
    content: serde_json::Value,
}

#[derive(Deserialize)]
struct ContextResponse {
    event: Option<ContextEventResponse>,
    #[serde(default)]
    state: Vec<ContextEventResponse>,
}

impl AppServiceMatrixDriver {
    /// Resolves the effective `m.room.member` presentation for `sender_mxid`
    /// at event `event_id`'s resolved room state context in `room_id`.
    ///
    /// Makes an authenticated call to `GET _matrix/client/v3/rooms/{roomId}/context/{eventId}?limit=0`
    /// to inspect the authoritative state at event $E$ without re-implementing
    /// state resolution inside Cumments.
    #[instrument(skip(self))]
    pub(super) async fn resolve_member_presentation_impl(
        &self,
        room_id: &str,
        event_id: &str,
        sender_mxid: &str,
    ) -> Result<Option<MemberPresentation>> {
        let path = format!(
            "_matrix/client/v3/rooms/{}/context/{}?limit=0",
            percent_encode(room_id),
            percent_encode(event_id)
        );
        let resp = self
            .request(reqwest::Method::GET, &path, None)
            .send()
            .await
            .map_err(|e| {
                anyhow!(
                    "Failed to fetch room context for event {} in {}: {}",
                    event_id,
                    room_id,
                    e
                )
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Context fetch for event {} in {} failed ({}): {}",
                event_id,
                room_id,
                status,
                error_body
            ));
        }

        let data: ContextResponse = resp.json().await.map_err(|e| {
            anyhow!(
                "Failed to parse context response for event {} in {}: {}",
                event_id,
                room_id,
                e
            )
        })?;

        let member_event = data
            .state
            .iter()
            .find(|evt| {
                evt.event_type == "m.room.member" && evt.state_key.as_deref() == Some(sender_mxid)
            })
            .or_else(|| {
                data.event.as_ref().filter(|evt| {
                    evt.event_type == "m.room.member"
                        && evt.state_key.as_deref() == Some(sender_mxid)
                })
            });

        let Some(member_event) = member_event else {
            return Ok(None);
        };

        let display_name = member_event
            .content
            .get("displayname")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let avatar_url = member_event
            .content
            .get("avatar_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        if display_name.is_none() && avatar_url.is_none() {
            Ok(None)
        } else {
            Ok(Some(MemberPresentation {
                display_name,
                avatar_url,
                media_reference: None,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appservice::test_support::test_driver;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn resolve_member_presentation_resolves_effective_state_at_event() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24event1%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "event": {
                    "type": "m.room.message",
                    "event_id": "$event1:hs",
                    "room_id": "!room:hs",
                    "content": { "body": "hello" }
                },
                "state": [
                    {
                        "type": "m.room.member",
                        "state_key": "@alice:hs",
                        "content": {
                            "membership": "join",
                            "displayname": "Alice at Event 1",
                            "avatar_url": "mxc://hs/alice1"
                        }
                    },
                    {
                        "type": "m.room.member",
                        "state_key": "@bob:hs",
                        "content": {
                            "membership": "join",
                            "displayname": "Bob",
                            "avatar_url": null
                        }
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let presentation = driver
            .resolve_member_presentation_impl("!room:hs", "$event1:hs", "@alice:hs")
            .await
            .expect("resolve succeeds");

        assert_eq!(
            presentation,
            Some(MemberPresentation {
                display_name: Some("Alice at Event 1".to_string()),
                avatar_url: Some("mxc://hs/alice1".to_string()),
                media_reference: None,
            })
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn resolve_member_presentation_returns_none_when_member_has_no_profile() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24event2%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "event": {
                    "type": "m.room.message",
                    "event_id": "$event2:hs",
                    "room_id": "!room:hs",
                    "content": { "body": "hello" }
                },
                "state": [
                    {
                        "type": "m.room.member",
                        "state_key": "@alice:hs",
                        "content": {
                            "membership": "leave"
                        }
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let presentation = driver
            .resolve_member_presentation_impl("!room:hs", "$event2:hs", "@alice:hs")
            .await
            .expect("resolve succeeds");

        assert_eq!(presentation, None);
        server.verify().await;
    }

    #[tokio::test]
    async fn resolve_member_presentation_returns_none_when_sender_not_in_state() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24event3%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "event": {
                    "type": "m.room.message",
                    "event_id": "$event3:hs",
                    "room_id": "!room:hs",
                    "content": { "body": "hello" }
                },
                "state": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let presentation = driver
            .resolve_member_presentation_impl("!room:hs", "$event3:hs", "@unknown:hs")
            .await
            .expect("resolve succeeds");

        assert_eq!(presentation, None);
        server.verify().await;
    }

    #[tokio::test]
    async fn resolve_member_presentation_returns_error_on_homeserver_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24missing%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "errcode": "M_NOT_FOUND",
                "error": "Event not found"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let err = driver
            .resolve_member_presentation_impl("!room:hs", "$missing:hs", "@alice:hs")
            .await
            .expect_err("not found must fail explicitly");

        assert!(err.to_string().contains("404"));
        server.verify().await;
    }

    #[tokio::test]
    async fn resolve_member_presentation_returns_error_on_homeserver_server_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24err%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal server error"))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let err = driver
            .resolve_member_presentation_impl("!room:hs", "$err:hs", "@alice:hs")
            .await
            .expect_err("500 must fail explicitly");

        assert!(err.to_string().contains("500"));
        server.verify().await;
    }
}
