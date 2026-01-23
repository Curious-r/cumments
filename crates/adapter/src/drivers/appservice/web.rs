use super::{handlers, AsContext};
use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use matrix_sdk::ruma::{
    events::{
        room::message::RoomMessageEvent,
        room::redaction::RoomRedactionEvent,
        AnyMessageLikeEvent, AnyTimelineEvent,
    },
    serde::Raw,
};
use serde::Deserialize;
use tracing::error;

#[derive(Deserialize)]
pub struct TransactionQuery {
    access_token: String,
}

#[derive(Deserialize, Debug)]
pub struct TransactionBody {
    events: Vec<Raw<AnyTimelineEvent>>,
}

pub async fn handle_transaction(
    State(ctx): State<AsContext>,
    Query(query): Query<TransactionQuery>,
    Path(_txn_id): Path<String>,
    Json(body): Json<TransactionBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // 校验 HS Token
    if query.access_token != ctx.config.hs_token {
        return Err(StatusCode::FORBIDDEN);
    }

    // 异步处理事件，不阻塞 HTTP 响应
    for raw_event in body.events {
        if let Ok(event) = raw_event.deserialize() {
            let ctx_clone = ctx.clone();
            tokio::spawn(async move {
                if let Err(e) = process_event(event, ctx_clone).await {
                    error!("AS Event Error: {:?}", e);
                }
            });
        }
    }

    Ok(Json(serde_json::json!({})))
}

async fn process_event(event: AnyTimelineEvent, ctx: AsContext) -> Result<()> {
    match event {
        AnyTimelineEvent::MessageLike(msg_event) => match msg_event {
            AnyMessageLikeEvent::RoomMessage(RoomMessageEvent::Original(ev)) => {
                handlers::handle_incoming_message(ev, &ctx).await
            }
            AnyMessageLikeEvent::RoomRedaction(RoomRedactionEvent::Original(ev)) => {
                handlers::handle_incoming_redaction(ev, &ctx).await
            }
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}
