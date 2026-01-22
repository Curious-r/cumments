use anyhow::Result;
use async_trait::async_trait;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::put,
    Json, Router,
};
use domain::{protocol, AppCommand, Comment, IngestEvent, SiteId};
use matrix_sdk::{
    matrix_auth::{MatrixSession, MatrixSessionTokens},
    ruma::{
        api::client::room::create_room::v3::Request as CreateRoomRequest,
        api::client::room::create_room::v3::RoomPreset,
        events::{
            room::message::{OriginalRoomMessageEvent, Relation, RoomMessageEvent},
            room::redaction::{OriginalRoomRedactionEvent, RoomRedactionEvent},
            AnyMessageLikeEvent, AnyTimelineEvent,
        },
        serde::Raw,
        EventId, OwnedRoomId, RoomAliasId, ServerName, UserId,
    },
    Client, SessionMeta,
};
use serde::Deserialize;
use std::net::SocketAddr;
use storage::Db;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use crate::common::matrix_utils::{compute_user_fingerprint, SpaceCache};
use crate::traits::MatrixDriver;
use crate::AppServiceConfig;

#[derive(Clone)]
struct AsContext {
    db: Db,
    tx_ingest: broadcast::Sender<IngestEvent>,
    config: AppServiceConfig,
}

pub struct AppServiceDriver {
    config: AppServiceConfig,
}

impl AppServiceDriver {
    pub fn new(config: AppServiceConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl MatrixDriver for AppServiceDriver {
    async fn run(
        &self,
        db: Db,
        mut rx_cmd: mpsc::Receiver<AppCommand>,
        tx_ingest: broadcast::Sender<IngestEvent>,
    ) -> Result<()> {
        info!(
            "Starting AppService Driver on port {}",
            self.config.listen_port
        );

        let main_client = Client::builder()
            .homeserver_url(&self.config.homeserver_url)
            .build()
            .await?;

        let main_user_id = UserId::parse(format!(
            "@{}:{}",
            self.config.bot_localpart, self.config.server_name
        ))?;

        let session = MatrixSession {
            meta: SessionMeta {
                user_id: main_user_id.clone(),
                device_id: "CUMMENTS_AS_BOT".into(),
            },
            tokens: MatrixSessionTokens {
                access_token: self.config.as_token.clone(),
                refresh_token: None,
            },
        };
        main_client.matrix_auth().restore_session(session).await?;
        info!("AS Main Bot logged in as {}", main_user_id);

        let space_cache = SpaceCache::new();

        let state = AsContext {
            db: db.clone(),
            tx_ingest: tx_ingest.clone(),
            config: self.config.clone(),
        };

        let app = Router::new()
            .route("/transactions/:txn_id", put(handle_transaction))
            .with_state(state);

        let addr = SocketAddr::from(([0, 0, 0, 0], self.config.listen_port));
        let listener = tokio::net::TcpListener::bind(addr).await?;

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                error!("AppService WebServer error: {}", e);
            }
        });

        info!("AppService listening for transactions on {}", addr);

        while let Some(cmd) = rx_cmd.recv().await {
            match cmd {
                AppCommand::SendComment {
                    site_id,
                    post_slug,
                    content,
                    nickname,
                    reply_to,
                    email,
                    guest_token,
                } => {
                    if let Err(e) = handle_as_send(
                        &main_client,
                        &self.config,
                        &db,
                        &space_cache,
                        &site_id,
                        &post_slug,
                        &nickname,
                        email.as_deref(),
                        &guest_token,
                        &content,
                        reply_to,
                    )
                    .await
                    {
                        error!("AS Send failed: {:?}", e);
                    }
                }
            }
        }

        Ok(())
    }
}

async fn handle_as_send(
    main_client: &Client,
    config: &AppServiceConfig,
    db: &Db,
    cache: &SpaceCache,
    site_id: &SiteId,
    slug: &str,
    nickname: &str,
    email: Option<&str>,
    guest_token: &str,
    content: &str,
    reply_to: Option<String>,
) -> Result<()> {
    let room_id = ensure_room_for_as(main_client, config, cache, site_id, slug).await?;
    db.ensure_room(room_id.as_str(), site_id.as_str(), slug)
        .await?;

    let fingerprint = compute_user_fingerprint(email, guest_token, &config.identity_salt);

    let ghost_localpart = format!("{}_{}", config.bot_localpart, fingerprint);
    let ghost_user_id = UserId::parse(format!("@{}:{}", ghost_localpart, config.server_name))?;

    let ghost_client = get_ghost_client(config, &ghost_user_id).await?;

    if ghost_client.get_room(&room_id).is_none() {
        ghost_client.join_room_by_id(&room_id).await?;
    }

    let _ = ghost_client
        .account()
        .set_display_name(Some(nickname))
        .await;

    let event_json = protocol::build_outbound_event(nickname, content, Some(fingerprint));
    let mut final_json = event_json;

    if let Some(parent_id_str) = reply_to {
        if let Ok(_) = EventId::parse(&parent_id_str) {
            if let Some(obj) = final_json.as_object_mut() {
                obj.insert(
                    "m.relates_to".to_string(),
                    serde_json::json!({ "m.in_reply_to": { "event_id": parent_id_str } }),
                );
            }
        }
    }

    if let Some(room) = ghost_client.get_room(&room_id) {
        use matrix_sdk::ruma::events::AnyMessageLikeEventContent;
        let raw_content: Raw<AnyMessageLikeEventContent> = serde_json::from_value(final_json)?;
        room.send_raw("m.room.message", raw_content).await?;
        info!("Sent AS message as {} ({})", ghost_user_id, nickname);
    } else {
        warn!("Ghost client joined room but get_room failed immediately.");
    }

    Ok(())
}

async fn get_ghost_client(config: &AppServiceConfig, user_id: &UserId) -> Result<Client> {
    let client = Client::builder()
        .homeserver_url(&config.homeserver_url)
        .build()
        .await?;

    let session = MatrixSession {
        meta: SessionMeta {
            user_id: user_id.to_owned(),
            device_id: "AS_GHOST".into(),
        },
        tokens: MatrixSessionTokens {
            access_token: config.as_token.clone(),
            refresh_token: None,
        },
    };

    client.matrix_auth().restore_session(session).await?;
    Ok(client)
}

async fn ensure_room_for_as(
    client: &Client,
    config: &AppServiceConfig,
    cache: &SpaceCache,
    site_id: &SiteId,
    slug: &str,
) -> Result<OwnedRoomId> {
    let full_alias = format!("#{}_{}:{}", site_id.as_str(), slug, config.server_name);
    let room_alias = RoomAliasId::parse(&full_alias)?;

    if let Ok(resp) = client.resolve_room_alias(&room_alias).await {
        return Ok(resp.room_id);
    }

    let space_id = crate::common::matrix_utils::ensure_site_space(
        client,
        &ServerName::parse(&config.server_name)?,
        cache,
        site_id,
    )
    .await?;

    let alias_local = format!("{}_{}", site_id.as_str(), slug);
    let mut req = CreateRoomRequest::new();
    req.room_alias_name = Some(alias_local);
    req.name = Some(format!("Comments for {}", slug));
    req.preset = Some(RoomPreset::PublicChat);

    info!("AS creating new room: {}", full_alias);
    let room = client.create_room(req).await?;
    let room_id = room.room_id().to_owned();

    if let Some(space_room) = client.get_room(&space_id) {
        use matrix_sdk::ruma::events::space::child::SpaceChildEventContent;
        let server_name = ServerName::parse(&config.server_name)?;
        let child = SpaceChildEventContent::new(vec![server_name.to_owned()]);
        let _ = space_room.send_state_event_for_key(&room_id, child).await;
    }

    Ok(room_id)
}

#[derive(Deserialize)]
struct TransactionQuery {
    access_token: String,
}

#[derive(Deserialize, Debug)]
struct TransactionBody {
    events: Vec<Raw<AnyTimelineEvent>>,
}

async fn handle_transaction(
    State(ctx): State<AsContext>,
    Query(query): Query<TransactionQuery>,
    Path(_txn_id): Path<String>,
    Json(body): Json<TransactionBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if query.access_token != ctx.config.hs_token {
        warn!("Unauthorized AS transaction attempt: invalid token");
        return Err(StatusCode::FORBIDDEN);
    }

    for raw_event in body.events {
        if let Ok(event) = raw_event.deserialize() {
            let ctx_clone = ctx.clone();
            tokio::spawn(async move {
                if let Err(e) = process_as_event(event, ctx_clone).await {
                    error!("Error processing AS event: {:?}", e);
                }
            });
        }
    }

    Ok(Json(serde_json::json!({})))
}

async fn process_as_event(event: AnyTimelineEvent, ctx: AsContext) -> Result<()> {
    match event {
        AnyTimelineEvent::MessageLike(msg_event) => match msg_event {
            AnyMessageLikeEvent::RoomMessage(RoomMessageEvent::Original(ev)) => {
                handle_as_message(ev, &ctx).await
            }
            AnyMessageLikeEvent::RoomRedaction(RoomRedactionEvent::Original(ev)) => {
                handle_as_redaction(ev, &ctx).await
            }
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}

async fn handle_as_message(event: OriginalRoomMessageEvent, ctx: &AsContext) -> Result<()> {
    let room_id_str = event.room_id.to_string();
    let sender_id = event.sender.to_string();

    let bot_exact = format!("@{}:{}", ctx.config.bot_localpart, ctx.config.server_name);
    let bot_prefix = format!("@{}_", ctx.config.bot_localpart);

    if sender_id == bot_exact || sender_id.starts_with(&bot_prefix) {
        return Ok(());
    }

    let (site_id, post_slug) = match ctx.db.get_room_meta(&room_id_str).await? {
        Some(meta) => meta,
        None => {
            warn!("AS received event in unknown room: {}", room_id_str);
            return Ok(());
        }
    };

    let content_json = serde_json::to_value(&event.content)?;

    let current_ts_millis: i64 = event.origin_server_ts.get().into();
    let current_time = chrono::DateTime::from_timestamp_millis(current_ts_millis)
        .unwrap_or_default()
        .naive_utc();

    let (target_id, final_content_json, updated_at) = if let Some(Relation::Replacement(ref re)) =
        event.content.relates_to
    {
        let original_id = re.event_id.to_string();
        let new_content_val = serde_json::to_value(&re.new_content).unwrap_or(content_json.clone());
        (original_id, new_content_val, Some(current_time))
    } else {
        (event.event_id.to_string(), content_json, None)
    };

    let (author_name, is_guest, content, author_fingerprint) =
        protocol::extract_comment_data(&final_content_json, &sender_id, &bot_exact);

    if content.trim().is_empty() {
        return Ok(());
    }

    let reply_to = if let Some(Relation::Reply { in_reply_to }) = event.content.relates_to {
        Some(in_reply_to.event_id.to_string())
    } else {
        None
    };

    let comment = Comment {
        id: target_id,
        site_id: site_id.clone(),
        post_slug: post_slug.clone(),
        author_id: sender_id,
        author_name,
        is_guest,
        is_redacted: false,
        author_fingerprint,
        content,
        created_at: current_time,
        updated_at,
        reply_to,
    };

    ctx.db
        .upsert_comment(&room_id_str, site_id.as_str(), &post_slug, &comment)
        .await?;
    info!("AS Comment received: {} -> {}", comment.id, comment.content);

    let _ = ctx.tx_ingest.send(IngestEvent::CommentSaved {
        site_id,
        post_slug,
        comment,
    });

    Ok(())
}

async fn handle_as_redaction(event: OriginalRoomRedactionEvent, ctx: &AsContext) -> Result<()> {
    if let Some(redacts_id) = event.redacts {
        let id_str = redacts_id.to_string();
        match ctx.db.delete_comment(&id_str).await {
            Ok(Some((site_id, post_slug))) => {
                info!("AS Redaction detected: {}", id_str);
                let _ = ctx.tx_ingest.send(IngestEvent::CommentDeleted {
                    site_id,
                    post_slug,
                    comment_id: id_str,
                });
            }
            Ok(None) => {}
            Err(e) => error!("Failed to delete comment: {:?}", e),
        }
    }
    Ok(())
}
