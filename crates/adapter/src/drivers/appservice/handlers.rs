use super::utils::{ensure_room_for_as, get_ghost_client};
use super::AsContext;
use crate::common::matrix_utils::SpaceCache;
use crate::common::profile::ensure_profile_cached;
use anyhow::Result;
use domain::identity::compute_fingerprint;
use domain::{protocol, Comment, IngestEvent, SiteId};
use matrix_sdk::ruma::{
    events::{
        relation::Replacement,
        room::message::{OriginalRoomMessageEvent, Relation, RoomMessageEventContent},
        room::redaction::OriginalRoomRedactionEvent,
        AnyMessageLikeEventContent,
    },
    serde::Raw,
    EventId, OwnedUserId, RoomAliasId, UserId,
};
use tracing::{info, warn};

pub async fn execute_send(
    ctx: &AsContext,
    cache: &SpaceCache,
    site_id: SiteId,
    post_slug: String,
    content: String,
    nickname: String,
    email: Option<String>,
    guest_token: String,
    reply_to: Option<String>,
    txn_id: Option<String>,
    owner_id: Option<&OwnedUserId>,
) -> Result<()> {
    let room_id = ensure_room_for_as(
        &ctx.main_client,
        &ctx.config,
        cache,
        &site_id,
        &post_slug,
        owner_id,
    )
    .await?;

    ctx.db
        .ensure_room(room_id.as_str(), site_id.as_str(), &post_slug)
        .await?;

    let fingerprint =
        compute_fingerprint(email.as_deref(), &guest_token, &ctx.config.identity_salt);

    let ghost_localpart = format!("{}_{}", ctx.config.bot_localpart, fingerprint);
    let ghost_user_id = UserId::parse(format!("@{}:{}", ghost_localpart, ctx.config.server_name))?;

    let ghost_client = get_ghost_client(&ctx.config, &ghost_user_id).await?;

    if ghost_client.get_room(&room_id).is_none() {
        ghost_client.join_room_by_id(&room_id).await?;
    }

    let _ = ghost_client
        .account()
        .set_display_name(Some(&nickname))
        .await;

    let event_json = protocol::build_outbound_event(&nickname, &content, Some(fingerprint), txn_id);
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
        let raw_content: Raw<AnyMessageLikeEventContent> = serde_json::from_value(final_json)?;
        room.send_raw("m.room.message", raw_content).await?;
        info!("AS Sent message as {} ({})", ghost_user_id, nickname);
    } else {
        warn!("Ghost client joined but get_room failed.");
    }

    Ok(())
}

pub async fn execute_redact(
    ctx: &AsContext,
    site_id: SiteId,
    slug: String,
    comment_id: String,
    reason: Option<String>,
) -> Result<()> {
    let alias_str = format!("#{}_{}:{}", site_id.as_str(), slug, ctx.config.server_name);
    let alias = RoomAliasId::parse(&alias_str)?;

    let resolve = ctx.main_client.resolve_room_alias(&alias).await?;
    if let Some(room) = ctx.main_client.get_room(&resolve.room_id) {
        if let Ok(eid) = EventId::parse(&comment_id) {
            room.redact(&eid, reason.as_deref(), None).await?;
        }
    }
    Ok(())
}

pub async fn execute_user_edit(
    ctx: &AsContext,
    site_id: SiteId,
    slug: String,
    comment_id: String,
    content: String,
    user_fingerprint: String,
) -> Result<()> {
    let comment_opt = ctx.db.get_comment(&comment_id).await?;
    let c = match comment_opt {
        Some(c) if c.author_fingerprint == Some(user_fingerprint) => c,
        _ => return Err(anyhow::anyhow!("Permission denied or comment not found")),
    };

    let alias_str = format!("#{}_{}:{}", site_id.as_str(), slug, ctx.config.server_name);
    let alias = RoomAliasId::parse(&alias_str)?;
    let resolve = ctx.main_client.resolve_room_alias(&alias).await?;
    let room_id = resolve.room_id;

    let author_uid = UserId::parse(&c.author_id)?;
    let ghost_client = get_ghost_client(&ctx.config, &author_uid).await?;

    let fallback_text = format!("* {}", content);
    let mut msg_content = RoomMessageEventContent::text_plain(fallback_text);

    msg_content.relates_to = Some(Relation::Replacement(Replacement::new(
        EventId::parse(&comment_id)?,
        RoomMessageEventContent::text_plain(content).into(),
    )));

    if let Some(room) = ghost_client.get_room(&room_id) {
        room.send(msg_content).await?;
    } else {
        let room = ghost_client.join_room_by_id(&room_id).await?;
        room.send(msg_content).await?;
    }

    Ok(())
}

pub async fn handle_incoming_message(
    raw_event: Raw<OriginalRoomMessageEvent>,
    ctx: &AsContext,
) -> Result<()> {
    let event = raw_event.deserialize()?;
    let room_id_str = event.room_id.to_string();
    let sender_id = event.sender.to_string();

    let bot_exact = format!("@{}:{}", ctx.config.bot_localpart, ctx.config.server_name);
    let bot_prefix = format!("@{}_", ctx.config.bot_localpart);

    if sender_id == bot_exact || sender_id.starts_with(&bot_prefix) {
        return Ok(());
    }

    let (site_id, post_slug) = match ctx.db.get_room_meta(&room_id_str).await? {
        Some(meta) => meta,
        None => return Ok(()),
    };

    let raw_json: serde_json::Value = raw_event.deserialize_as()?;
    let content_json = raw_json
        .get("content")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let current_ts_millis: i64 = event.origin_server_ts.get().into();
    let current_time = chrono::DateTime::from_timestamp_millis(current_ts_millis)
        .map(|dt| dt.naive_utc())
        .unwrap_or_else(|| chrono::Utc::now().naive_utc());

    let (target_id, final_content_json, updated_at) =
        if let Some(Relation::Replacement(ref re)) = event.content.relates_to {
            let original_id = re.event_id.to_string();
            let new_content_val =
                serde_json::to_value(&re.new_content).unwrap_or_else(|_| content_json.clone());
            (original_id, new_content_val, Some(current_time))
        } else {
            (event.event_id.to_string(), content_json, None)
        };

    let (mut author_name, is_guest, content, author_fingerprint, txn_id) =
        protocol::extract_comment_data(&final_content_json, &sender_id, &bot_exact);

    if content.trim().is_empty() {
        return Ok(());
    }

    let mut avatar_url = None;
    if !is_guest {
        let (name, avatar) =
            ensure_profile_cached(&ctx.db, &ctx.main_client, None, &sender_id).await;
        author_name = name;
        avatar_url = avatar;
    }

    let reply_to = if let Some(Relation::Reply { ref in_reply_to }) = event.content.relates_to {
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
        avatar_url,
        is_guest,
        is_redacted: false,
        author_fingerprint,
        content,
        created_at: current_time,
        updated_at,
        reply_to,
        txn_id,
    };

    let raw_event_str = serde_json::to_string(&final_content_json).ok();

    ctx.db
        .upsert_comment(
            &room_id_str,
            site_id.as_str(),
            &post_slug,
            &comment,
            raw_event_str,
        )
        .await?;

    let _ = ctx.tx_ingest.send(IngestEvent::CommentSaved {
        site_id,
        post_slug,
        comment,
    });

    Ok(())
}

pub async fn handle_incoming_redaction(
    event: OriginalRoomRedactionEvent,
    ctx: &AsContext,
) -> Result<()> {
    if let Some(redacts_id) = event.redacts {
        let id_str = redacts_id.to_string();
        if let Ok(Some((site_id, post_slug))) = ctx.db.delete_comment(&id_str).await {
            info!("AS Redacted: {}", id_str);
            let _ = ctx.tx_ingest.send(IngestEvent::CommentDeleted {
                site_id,
                post_slug,
                comment_id: id_str,
            });
        }
    }
    Ok(())
}

pub async fn execute_ensure_room(
    ctx: &AsContext,
    cache: &SpaceCache,
    site_id: SiteId,
    post_slug: String,
    owner_id: Option<&OwnedUserId>,
) -> Result<()> {
    let room_id = ensure_room_for_as(
        &ctx.main_client,
        &ctx.config,
        cache,
        &site_id,
        &post_slug,
        owner_id,
    )
    .await?;

    ctx.db
        .ensure_room(room_id.as_str(), site_id.as_str(), &post_slug)
        .await?;

    info!("AS Room ensured for {}/{}", site_id, post_slug);
    Ok(())
}
