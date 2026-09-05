//! End-to-end creation round-trip: a semantic creation input is signed with
//! the canonical visitor signature, encoded into a Matrix event by the real
//! wire builders, delivered as an AppService push transaction, interpreted by
//! the relation parser, and projected into the read model. The relations that
//! come back must be exactly the relations that went in — with fallback-only
//! `m.in_reply_to` never becoming a Cumments `reply_to`.

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use cumments_core::identity::{derive_visitor_id_from_public_key, post_signature_message};
use cumments_core::models::ThreadSummary;
use cumments_core::ports::{AppServiceTxnStore, MessageStore, RegistryStore, SseOutboxStore};
use cumments_core::protocol::MESSAGE_CONTENT_KEY;
use cumments_matrix::build_message_body;
use cumments_projector::event_processor::{EventProcessor, EventProcessorDeps};
use cumments_projector::push_receiver::push_router;
use cumments_store::DbStore;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::broadcast;
use tower::ServiceExt;

fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-thread-roundtrip-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

const HS_TOKEN: &str = "hs-token";

async fn harness(name: &str) -> (Arc<DbStore>, Router) {
    let store = Arc::new(
        DbStore::connect(&test_db_url(name))
            .await
            .expect("connect db"),
    );
    store
        .register_room(
            "!room:hs",
            &cumments_core::models::SiteId::from("my-blog"),
            &cumments_core::models::PageSlug::from("hello"),
        )
        .await
        .expect("register room");

    let (event_bus, _rx) = broadcast::channel(16);
    let processor = Arc::new(EventProcessor::new(EventProcessorDeps {
        site_store: store.clone() as Arc<dyn cumments_core::ports::SiteStore>,
        registry_store: store.clone() as Arc<dyn cumments_core::ports::RegistryStore>,
        message_store: store.clone() as Arc<dyn cumments_core::ports::MessageStore>,
        room_store: store.clone() as Arc<dyn cumments_core::ports::RoomStore>,
        governance_store: store.clone() as Arc<dyn cumments_core::ports::GovernanceStore>,
        sticker_pack_store: store.clone() as Arc<dyn cumments_core::ports::StickerPackStore>,
        projection_repair_store: store.clone()
            as Arc<dyn cumments_core::ports::ProjectionRepairStore>,
        role_claim_store: store.clone() as Arc<dyn cumments_core::ports::RoleClaimStore>,
        submission_store: store.clone() as Arc<dyn cumments_core::ports::SubmissionStore>,
        audit_store: store.clone() as Arc<dyn cumments_core::ports::CommandAuditStore>,
        site_auth_store: store.clone() as Arc<dyn cumments_core::ports::SiteAuthStore>,
        site_auth_policy: Arc::new(cumments_core::site_auth::SiteAuthPolicy {
            verification: cumments_core::site_auth::SiteVerificationPolicy::Optional,
            sites: Default::default(),
        }),
        site_service: Arc::new(cumments_core::site_service::SiteService::new(
            store.clone() as Arc<dyn cumments_core::ports::SiteStore>
        )),
        driver: None,
        operator_mxids: Vec::new(),
        backfill_tx: None,
        event_bus,
        governance_notify: Arc::new(tokio::sync::Notify::new()),
        projection_notify: Arc::new(tokio::sync::Notify::new()),
        server_name: Some("hs".to_string()),
    }));

    let app = push_router(
        processor,
        store.clone() as Arc<dyn AppServiceTxnStore>,
        store.clone() as Arc<dyn SseOutboxStore>,
        HS_TOKEN.to_string(),
    );
    (store, app)
}

/// Deliver one event as an AppService push transaction, as the homeserver
/// would after accepting the driver's send request.
async fn push_event(app: &Router, txn_id: &str, event: Value) -> StatusCode {
    let transaction = json!({ "events": [event] });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/_matrix/app/v1/transactions/{txn_id}"))
                .header(header::AUTHORIZATION, format!("Bearer {HS_TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(transaction.to_string()))
                .expect("build request"),
        )
        .await
        .expect("push transaction");
    response.status()
}

fn native_text_event(event_id: &str, sender: &str, ts: i64, content: Value) -> Value {
    json!({
        "type": "m.room.message",
        "event_id": event_id,
        "room_id": "!room:hs",
        "sender": sender,
        "origin_server_ts": ts,
        "content": content,
    })
}

/// Build a visitor-signed Matrix event through the real wire builder: the
/// semantic relations go into the signature (post_signature_message) and the
/// encoded `m.relates_to`, exactly as the reconciler's MatrixDriver would
/// produce for a queued PostCommentCommand.
fn visitor_text_event(
    event_id: &str,
    ts: i64,
    signing_key: &SigningKey,
    reply_to: Option<&str>,
    thread_root: Option<&str>,
) -> Value {
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let visitor_id = derive_visitor_id_from_public_key(&public_key).expect("visitor id");
    let sender = format!("@_cumments_my-blog_{visitor_id}:hs");
    let challenge = "chal";
    let message = post_signature_message(
        "my-blog",
        "hello",
        "a reply",
        reply_to,
        thread_root,
        challenge,
    );
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());
    let content = build_message_body(
        "a reply",
        &public_key,
        &signature,
        challenge,
        None,
        reply_to,
        thread_root,
        None,
        None,
    );
    native_text_event(event_id, &sender, ts, content)
}

#[tokio::test]
async fn visitor_creation_relations_round_trip_through_the_push_path() {
    let (store, app) = harness("visitor-roundtrip").await;

    // Context: a Thread root candidate and an unrelated top-level comment.
    // `$parent:hs` deliberately lives outside any Thread so the thread+reply
    // case proves the two relations stay independent end to end.
    for (event_id, ts, txn_id) in [
        ("$root:hs", 100, "txn-root"),
        ("$parent:hs", 110, "txn-parent"),
    ] {
        assert_eq!(
            push_event(
                &app,
                txn_id,
                native_text_event(
                    event_id,
                    "@alice:hs",
                    ts,
                    json!({ "msgtype": "m.text", "body": event_id }),
                ),
            )
            .await,
            StatusCode::OK,
            "seeding {event_id} must project"
        );
    }

    let signing_key = SigningKey::from_bytes(&[23u8; 32]);

    // Thread only: Thread membership without a genuine direct reply.
    assert_eq!(
        push_event(
            &app,
            "txn-thread-only",
            visitor_text_event("$a:hs", 200, &signing_key, None, Some("$root:hs")),
        )
        .await,
        StatusCode::OK,
        "the push path must accept and project a thread-only visitor event"
    );
    let member = store
        .get_message("$a:hs")
        .await
        .expect("get member")
        .expect("member projected");
    assert_eq!(member.thread_root.as_deref(), Some("$root:hs"));
    assert!(
        member.reply_to.is_none(),
        "thread membership must not imply a direct reply"
    );
    assert_eq!(
        member.author.kind,
        cumments_core::models::AuthorKind::Visitor
    );
    let root = store
        .get_message("$root:hs")
        .await
        .expect("get root")
        .expect("root projected");
    assert_eq!(
        root.thread_summary,
        Some(ThreadSummary {
            num_replies: 1,
            latest_reply: Some("$a:hs".to_string())
        })
    );

    // Thread + genuine reply: both relations survive the wire round trip.
    assert_eq!(
        push_event(
            &app,
            "txn-thread-and-reply",
            visitor_text_event(
                "$b:hs",
                300,
                &signing_key,
                Some("$parent:hs"),
                Some("$root:hs")
            ),
        )
        .await,
        StatusCode::OK
    );
    let member = store
        .get_message("$b:hs")
        .await
        .expect("get member")
        .expect("member projected");
    assert_eq!(member.thread_root.as_deref(), Some("$root:hs"));
    assert_eq!(
        member.reply_to.as_deref(),
        Some("$parent:hs"),
        "the genuine direct parent must survive encoding, parsing, and projection"
    );
    let root = store
        .get_message("$root:hs")
        .await
        .expect("get root")
        .expect("root projected");
    assert_eq!(
        root.thread_summary,
        Some(ThreadSummary {
            num_replies: 2,
            latest_reply: Some("$b:hs".to_string())
        })
    );

    // Reply without Thread: an ordinary direct reply outside any Thread,
    // which must not contribute to the root's summary.
    assert_eq!(
        push_event(
            &app,
            "txn-reply-only",
            visitor_text_event("$c:hs", 400, &signing_key, Some("$parent:hs"), None),
        )
        .await,
        StatusCode::OK
    );
    let member = store
        .get_message("$c:hs")
        .await
        .expect("get member")
        .expect("member projected");
    assert_eq!(member.reply_to.as_deref(), Some("$parent:hs"));
    assert!(member.thread_root.is_none());
    let root = store
        .get_message("$root:hs")
        .await
        .expect("get root")
        .expect("root projected");
    assert_eq!(
        root.thread_summary.as_ref().expect("summary").num_replies,
        2
    );

    // The durable SSE outbox received the projected member events: the push
    // path captures the projector's events into the outbox for publication
    // after commit.
    let outbox = store.pending_sse_outbox(100).await.expect("outbox");
    assert!(
        outbox.iter().any(|row| row
            .payload_json
            .as_deref()
            .is_some_and(|payload| payload.contains("message_created"))),
        "member creations must reach the SSE outbox"
    );
}

#[tokio::test]
async fn fallback_and_genuine_in_thread_replies_stay_distinct_end_to_end() {
    let (store, app) = harness("native-roundtrip").await;

    assert_eq!(
        push_event(
            &app,
            "txn-root",
            native_text_event(
                "$root:hs",
                "@alice:hs",
                100,
                json!({ "msgtype": "m.text", "body": "$root:hs" }),
            ),
        )
        .await,
        StatusCode::OK
    );

    // A Matrix client that does not understand Threads sends its in-thread
    // event through the reply fallback: `m.in_reply_to` names a fallback
    // target and `is_falling_back` marks it as such. The relation must
    // project as Thread membership only.
    assert_eq!(
        push_event(
            &app,
            "txn-fallback",
            native_text_event(
                "$fallback:hs",
                "@bob:hs",
                200,
                json!({
                    "msgtype": "m.text",
                    "body": "in thread via fallback",
                    "m.relates_to": {
                        "rel_type": "m.thread",
                        "event_id": "$root:hs",
                        "m.in_reply_to": { "event_id": "$fallback-target:hs" },
                        "is_falling_back": true,
                    },
                }),
            ),
        )
        .await,
        StatusCode::OK
    );
    let member = store
        .get_message("$fallback:hs")
        .await
        .expect("get fallback member")
        .expect("member projected");
    assert_eq!(member.thread_root.as_deref(), Some("$root:hs"));
    assert_eq!(
        member.reply_to, None,
        "a fallback target must never become the Cumments reply_to"
    );

    // A genuine in-thread reply from a Thread-aware client carries the actual
    // parent without the fallback marker and projects as both relations.
    assert_eq!(
        push_event(
            &app,
            "txn-genuine",
            native_text_event(
                "$genuine:hs",
                "@bob:hs",
                300,
                json!({
                    "msgtype": "m.text",
                    "body": "genuine in-thread reply",
                    "m.relates_to": {
                        "rel_type": "m.thread",
                        "event_id": "$root:hs",
                        "m.in_reply_to": { "event_id": "$fallback:hs" },
                        "is_falling_back": false,
                    },
                }),
            ),
        )
        .await,
        StatusCode::OK
    );
    let member = store
        .get_message("$genuine:hs")
        .await
        .expect("get genuine member")
        .expect("member projected");
    assert_eq!(member.thread_root.as_deref(), Some("$root:hs"));
    assert_eq!(member.reply_to.as_deref(), Some("$fallback:hs"));

    let root = store
        .get_message("$root:hs")
        .await
        .expect("get root")
        .expect("root projected");
    assert_eq!(
        root.thread_summary,
        Some(ThreadSummary {
            num_replies: 2,
            latest_reply: Some("$genuine:hs".to_string())
        })
    );
    // The proof block is never present on Matrix-native events.
    assert!(
        !serde_json::to_string(&member.raw_content)
            .expect("raw content")
            .contains(MESSAGE_CONTENT_KEY),
        "native events must not gain a Cumments proof block"
    );
}
