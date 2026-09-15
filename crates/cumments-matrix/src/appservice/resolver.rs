use anyhow::{Result, anyhow};
use cumments_core::models::MemberPresentation;
use serde::Deserialize;
use tracing::instrument;

use crate::appservice::AppServiceMatrixDriver;
use crate::wire::percent_encode;

#[derive(Deserialize, Debug, Clone)]
struct ContextEventResponse {
    #[serde(rename = "type")]
    event_type: String,
    state_key: Option<String>,
    #[serde(default)]
    content: serde_json::Value,
}

#[derive(Deserialize, Debug, Clone)]
struct ContextResponse {
    event: Option<ContextEventResponse>,
    #[serde(default)]
    state: Vec<ContextEventResponse>,
}

impl AppServiceMatrixDriver {
    /// Resolves the effective `m.room.member` presentation for `sender_mxid`
    /// at event `event_id`'s resolved room state context in `room_id`.
    ///
    /// # Matrix Facility & Semantics
    /// Makes an authenticated call to:
    /// `GET /_matrix/client/v3/rooms/{roomId}/context/{eventId}?limit=0`
    ///
    /// ## Guarantees & Constraints
    /// The Matrix Client-Server API specification defines that `GET /context/{eventId}`
    /// returns the state of the room at the point present for the event. With `limit=0`,
    /// no timeline events before or after are retrieved; the returned `state` list contains
    /// the room state events at event $E$'s DAG position as resolved by the homeserver's
    /// Matrix State Resolution algorithm.
    ///
    /// ## Member State Interpretation
    /// 1. **No member state**: If `state` contains no `m.room.member` event for `sender_mxid`
    ///    (and event $E$ itself is not the sender's member event), returns `Ok(None)`.
    /// 2. **Malformed state**: If the `m.room.member` event has missing or invalid `membership`,
    ///    returns `Err(...)`.
    /// 3. **Usable presentation**: If the resolved member state contains non-empty `displayname`
    ///    or `avatar_url`, returns `Ok(Some(MemberPresentation))` regardless of whether
    ///    `membership` is `join`, `leave`, or `ban`. Presentation usability is not restricted
    ///    to joined members.
    /// 4. **No usable presentation**: If the member state exists but `displayname` and `avatar_url`
    ///    are absent, null, or empty, returns `Ok(None)`.
    /// 5. **Explicit clearing**: If a profile field was cleared (unset/null), it is interpreted as
    ///    `None` directly from the resolved state; no attempt is made to guess past values or fall
    ///    back to current room state.
    ///
    /// ## Failure Modes
    /// If the homeserver returns a 4xx/5xx status (e.g. 403 Forbidden due to history visibility,
    /// 404 Not Found if pruned), transport error, or malformed JSON, returns `Err(...)`.
    /// Resolver failures are never converted to `Ok(None)`.
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

        // Validate membership field
        let membership = member_event
            .content
            .get("membership")
            .and_then(|v| v.as_str());

        let Some(membership) = membership else {
            return Err(anyhow!(
                "Malformed m.room.member event for {} in context of event {} in room {}: missing or non-string 'membership'",
                sender_mxid,
                event_id,
                room_id
            ));
        };

        match membership {
            "join" | "leave" | "ban" | "invite" | "knock" => {}
            other => {
                return Err(anyhow!(
                    "Malformed m.room.member event for {} in context of event {} in room {}: unrecognized membership '{}'",
                    sender_mxid,
                    event_id,
                    room_id,
                    other
                ));
            }
        }

        let display_name = member_event
            .content
            .get("displayname")
            .and_then(|v| if v.is_null() { None } else { v.as_str() })
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned);

        let avatar_url = member_event
            .content
            .get("avatar_url")
            .and_then(|v| if v.is_null() { None } else { v.as_str() })
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned);

        if display_name.is_none() && avatar_url.is_none() {
            Ok(None)
        } else {
            Ok(Some(MemberPresentation {
                display_name,
                avatar_url,
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
            })
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn resolve_member_presentation_resolves_leave_with_usable_profile() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24event-leave%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "event": {
                    "type": "m.room.message",
                    "event_id": "$event-leave:hs",
                    "room_id": "!room:hs",
                    "content": { "body": "farewell" }
                },
                "state": [
                    {
                        "type": "m.room.member",
                        "state_key": "@alice:hs",
                        "content": {
                            "membership": "leave",
                            "displayname": "Alice Left",
                            "avatar_url": "mxc://hs/alice-left"
                        }
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let presentation = driver
            .resolve_member_presentation_impl("!room:hs", "$event-leave:hs", "@alice:hs")
            .await
            .expect("resolve succeeds");

        assert_eq!(
            presentation,
            Some(MemberPresentation {
                display_name: Some("Alice Left".to_string()),
                avatar_url: Some("mxc://hs/alice-left".to_string()),
            })
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn resolve_member_presentation_resolves_ban_with_usable_profile() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24event-ban%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "event": {
                    "type": "m.room.message",
                    "event_id": "$event-ban:hs",
                    "room_id": "!room:hs",
                    "content": { "body": "message before ban" }
                },
                "state": [
                    {
                        "type": "m.room.member",
                        "state_key": "@alice:hs",
                        "content": {
                            "membership": "ban",
                            "displayname": "Banned Alice",
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
            .resolve_member_presentation_impl("!room:hs", "$event-ban:hs", "@alice:hs")
            .await
            .expect("resolve succeeds");

        assert_eq!(
            presentation,
            Some(MemberPresentation {
                display_name: Some("Banned Alice".to_string()),
                avatar_url: None,
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
    async fn resolve_member_presentation_returns_none_when_fields_are_null_or_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24event-cleared%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "event": {
                    "type": "m.room.message",
                    "event_id": "$event-cleared:hs",
                    "room_id": "!room:hs",
                    "content": { "body": "hello" }
                },
                "state": [
                    {
                        "type": "m.room.member",
                        "state_key": "@alice:hs",
                        "content": {
                            "membership": "join",
                            "displayname": null,
                            "avatar_url": ""
                        }
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let presentation = driver
            .resolve_member_presentation_impl("!room:hs", "$event-cleared:hs", "@alice:hs")
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
    async fn resolve_member_presentation_resolves_when_target_event_is_member_event() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24event-member%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "event": {
                    "type": "m.room.member",
                    "state_key": "@alice:hs",
                    "content": {
                        "membership": "join",
                        "displayname": "Alice Direct",
                        "avatar_url": "mxc://hs/alice-direct"
                    }
                },
                "state": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let presentation = driver
            .resolve_member_presentation_impl("!room:hs", "$event-member:hs", "@alice:hs")
            .await
            .expect("resolve succeeds");

        assert_eq!(
            presentation,
            Some(MemberPresentation {
                display_name: Some("Alice Direct".to_string()),
                avatar_url: Some("mxc://hs/alice-direct".to_string()),
            })
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn resolve_member_presentation_returns_error_on_missing_membership() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24event-bad-state%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "event": {
                    "type": "m.room.message",
                    "event_id": "$event-bad-state:hs",
                    "room_id": "!room:hs",
                    "content": { "body": "hello" }
                },
                "state": [
                    {
                        "type": "m.room.member",
                        "state_key": "@alice:hs",
                        "content": {
                            "displayname": "Alice Without Membership"
                        }
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let err = driver
            .resolve_member_presentation_impl("!room:hs", "$event-bad-state:hs", "@alice:hs")
            .await
            .expect_err("missing membership must return error");

        assert!(err.to_string().contains("membership"));
        server.verify().await;
    }

    #[tokio::test]
    async fn resolve_member_presentation_returns_error_on_invalid_membership() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24event-inv-mem%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "event": {
                    "type": "m.room.message",
                    "event_id": "$event-inv-mem:hs",
                    "room_id": "!room:hs",
                    "content": { "body": "hello" }
                },
                "state": [
                    {
                        "type": "m.room.member",
                        "state_key": "@alice:hs",
                        "content": {
                            "membership": "invalid_membership_type",
                            "displayname": "Alice"
                        }
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let err = driver
            .resolve_member_presentation_impl("!room:hs", "$event-inv-mem:hs", "@alice:hs")
            .await
            .expect_err("invalid membership must return error");

        assert!(err.to_string().contains("unrecognized membership"));
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
    async fn resolve_member_presentation_returns_error_on_homeserver_forbidden() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24forbidden%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "errcode": "M_FORBIDDEN",
                "error": "You do not have permission to view context at this event"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let err = driver
            .resolve_member_presentation_impl("!room:hs", "$forbidden:hs", "@alice:hs")
            .await
            .expect_err("forbidden must fail explicitly");

        assert!(err.to_string().contains("403"));
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

    #[tokio::test]
    async fn resolve_member_presentation_returns_error_on_malformed_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/%21room%3Ahs/context/%24corrupt%3Ahs",
            ))
            .and(query_param("limit", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not valid json"))
            .expect(1)
            .mount(&server)
            .await;

        let driver = test_driver(&server);
        let err = driver
            .resolve_member_presentation_impl("!room:hs", "$corrupt:hs", "@alice:hs")
            .await
            .expect_err("malformed JSON must fail explicitly");

        assert!(err.to_string().contains("Failed to parse context response"));
        server.verify().await;
    }
}
