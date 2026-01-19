use anyhow::Result;
use async_trait::async_trait;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::put,
    Json, Router,
};
use domain::{protocol, AppCommand, Comment, IngestEvent};
use matrix_sdk::ruma::{
    events::{
        room::message::{OriginalRoomMessageEvent, Relation, RoomMessageEvent},
        room::redaction::{OriginalRoomRedactionEvent, RoomRedactionEvent},
        AnyMessageLikeEvent, AnyTimelineEvent,
    },
    serde::Raw,
};
use serde::Deserialize;
use std::net::SocketAddr;
use storage::Db;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

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
                AppCommand::SendComment { .. } => {
                    warn!("AS Mode SendComment not implemented yet!");
                }
            }
        }

        Ok(())
    }
}

// --- HTTP Handlers ---

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

    let bot_localpart = &ctx.config.bot_localpart;
    if sender_id.starts_with(&format!("@{}:", bot_localpart))
        || sender_id.contains(&format!(":{}_", bot_localpart))
    {
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

    let (author_name, is_guest, content) = protocol::extract_comment_data(
        &final_content_json,
        &sender_id,
        &format!("@{}:{}", bot_localpart, ctx.config.server_name),
    );

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
        content,
        created_at: current_time,
        updated_at,
        reply_to,
    };

    ctx.db.upsert_comment(&room_id_str, &comment).await?;
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
