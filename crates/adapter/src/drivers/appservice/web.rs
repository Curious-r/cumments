use super::{handlers, AsContext};
use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use matrix_sdk::ruma::{
    events::{
        room::message::RoomMessageEvent, room::redaction::RoomRedactionEvent, AnyMessageLikeEvent,
        AnyTimelineEvent,
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
    if query.access_token != ctx.config.hs_token {
        return Err(StatusCode::FORBIDDEN);
    }

    for raw_event in body.events {
        let ctx_clone = ctx.clone();
        tokio::spawn(async move {
            if let Err(e) = process_event(raw_event, ctx_clone).await {
                error!("Error processing AS event: {:?}", e);
            }
        });
    }

    Ok(Json(serde_json::json!({})))
}

async fn process_event(raw_event: Raw<AnyTimelineEvent>, ctx: AsContext) -> Result<()> {
    let event = match raw_event.deserialize() {
        Ok(e) => e,
        Err(e) => {
            return Err(e.into());
        }
    };

    match event {
        AnyTimelineEvent::MessageLike(msg_event) => match msg_event {
            AnyMessageLikeEvent::RoomMessage(RoomMessageEvent::Original(_)) => {
                handlers::handle_incoming_message(raw_event.cast(), &ctx).await
            }
            AnyMessageLikeEvent::RoomRedaction(RoomRedactionEvent::Original(ev)) => {
                handlers::handle_incoming_redaction(ev, &ctx).await
            }
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}
