use anyhow::Result;
use domain::{protocol, Comment, IngestEvent, SiteId};
use matrix_sdk::{
    ruma::{
        api::client::alias::delete_alias::v3::Request as DeleteAliasRequest,
        events::{
            room::message::{OriginalSyncRoomMessageEvent, Relation},
            AnyMessageLikeEventContent,
        },
        serde::Raw,
        EventId, RoomAliasId, ServerName,
    },
    Client, Room,
};
use storage::Db;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::common::matrix_utils::{
    create_and_link_room, ensure_site_space, resolve_room_alias_chain, SpaceCache,
};

fn resolve_event_details(
    event: &OriginalSyncRoomMessageEvent,
    content_json: serde_json::Value,
) -> (
    String,
    serde_json::Value,
    chrono::NaiveDateTime,
    Option<chrono::NaiveDateTime>,
) {
    let current_ts_millis: i64 = event.origin_server_ts.get().into();
    let current_time = chrono::DateTime::from_timestamp_millis(current_ts_millis)
        .unwrap_or_default()
        .naive_utc();

    if let Some(Relation::Replacement(ref re)) = event.content.relates_to {
        let original_id = re.event_id.to_string();
        let new_content_val = serde_json::to_value(&re.new_content).unwrap_or(content_json);
        (
            original_id,
            new_content_val,
            current_time,
            Some(current_time),
        )
    } else {
        (event.event_id.to_string(), content_json, current_time, None)
    }
}

pub async fn handle_sync_event(
    event: OriginalSyncRoomMessageEvent,
    room: Room,
    client: Client,
    db: Db,
    bot_id: String,
    tx: broadcast::Sender<IngestEvent>,
) -> Result<()> {
    let alias_str = match resolve_room_alias_chain(&room, &client).await {
        Some(a) => a,
        None => {
            warn!("Ignored event in room {} (No alias found)", room.room_id());
            return Ok(());
        }
    };

    let localpart = alias_str
        .split(':')
        .next()
        .unwrap_or("")
        .trim_start_matches('#');
    let (site_id, post_slug) = match protocol::parse_room_alias(localpart) {
        Some(res) => res,
        None => {
            warn!(
                "Ignored event in room {} (Invalid alias fmt: {})",
                room.room_id(),
                localpart
            );
            return Ok(());
        }
    };

    let content_json = serde_json::to_value(&event.content)?;
    let (target_id, final_content_json, current_time, updated_at) =
        resolve_event_details(&event, content_json);

    let sender_id = event.sender.to_string();
    let (author_name, is_guest, content) =
        protocol::extract_comment_data(&final_content_json, &sender_id, &bot_id);

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

    let room_id = room.room_id().as_str();
    db.ensure_room(room_id, site_id.as_str(), &post_slug)
        .await?;

    db.upsert_comment(room_id, &comment).await?;

    info!("Comment synced: {} -> {}", comment.id, comment.content);

    let _ = tx.send(IngestEvent::CommentSaved {
        site_id,
        post_slug,
        comment,
    });

    Ok(())
}

pub async fn handle_multitenant_send(
    client: &Client,
    server_name: &ServerName,
    db: &Db,
    cache: &SpaceCache,
    site_id: &SiteId,
    slug: &str,
    event_json: serde_json::Value,
    reply_to: Option<String>,
) -> Result<()> {
    let space_id = ensure_site_space(client, server_name, cache, site_id).await?;

    let full_alias = format!("#{}_{}:{}", site_id.as_str(), slug, server_name);
    let room_alias = RoomAliasId::parse(&full_alias)?;

    let room = match client.resolve_room_alias(&room_alias).await {
        Ok(resp) => match client.get_room(&resp.room_id) {
            Some(r) => r,
            None => match client.join_room_by_id(&resp.room_id).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        "Alias {} exists but join failed: {:?}. Recreating.",
                        room_alias, e
                    );
                    let req = DeleteAliasRequest::new(room_alias.clone());
                    client.send(req, None).await?;
                    create_and_link_room(client, server_name, &space_id, site_id, slug).await?
                }
            },
        },
        Err(_) => create_and_link_room(client, server_name, &space_id, site_id, slug).await?,
    };

    // [关键变更] 确保房间已注册 (Write Path)
    // 无论是刚创建的还是查找出来的，都确保 DB 里有记录
    db.ensure_room(room.room_id().as_str(), site_id.as_str(), slug)
        .await?;

    let mut final_json = event_json;
    if let Some(parent_id_str) = reply_to {
        if let Ok(_) = EventId::parse(&parent_id_str) {
            if let Some(obj) = final_json.as_object_mut() {
                obj.insert(
                    "m.relates_to".to_string(),
                    serde_json::json!({ "m.in_reply_to": { "event_id": parent_id_str } }),
                );
            }
        } else {
            error!("Invalid reply_to ID: {}", parent_id_str);
        }
    }

    let raw_content: Raw<AnyMessageLikeEventContent> = serde_json::from_value(final_json)?;
    room.send_raw("m.room.message", raw_content).await?;
    Ok(())
}
