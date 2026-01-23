use super::utils::{ensure_room_for_as, get_ghost_client};
use super::AsContext;
use crate::common::matrix_utils::{compute_user_fingerprint, SpaceCache};
use anyhow::Result;
use domain::{protocol, Comment, IngestEvent, SiteId};
use matrix_sdk::ruma::{
    events::{
        room::message::{OriginalRoomMessageEvent, Relation, RoomMessageEventContent},
        room::redaction::OriginalRoomRedactionEvent,
        AnyMessageLikeEventContent,
        relation::Replacement,
    },
    serde::Raw,
    EventId, OwnedUserId, RoomAliasId, UserId,
};
use tracing::{info, warn};

// ================================================================================================
//  Outgoing: 处理来自 Domain 层的命令
// ================================================================================================

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
    // 1. 确保房间存在
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

    // 2. 计算指纹并伪装 Ghost
    let fingerprint =
        compute_user_fingerprint(email.as_deref(), &guest_token, &ctx.config.identity_salt);

    let ghost_localpart = format!("{}_{}", ctx.config.bot_localpart, fingerprint);
    let ghost_user_id =
        UserId::parse(format!("@{}:{}", ghost_localpart, ctx.config.server_name))?;

    let ghost_client = get_ghost_client(&ctx.config, &ghost_user_id).await?;

    // 确保 Ghost 加入房间
    if ghost_client.get_room(&room_id).is_none() {
        ghost_client.join_room_by_id(&room_id).await?;
    }

    // 设置 Ghost 昵称
    let _ = ghost_client
        .account()
        .set_display_name(Some(&nickname))
        .await;

    // 3. 构建并发送消息
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

// 通用的撤回逻辑
pub async fn execute_redact(
    ctx: &AsContext,
    site_id: SiteId,
    slug: String,
    comment_id: String,
    reason: Option<String>,
) -> Result<()> {
    let alias_str = format!("#{}_{}:{}", site_id.as_str(), slug, ctx.config.server_name);
    let alias = RoomAliasId::parse(&alias_str)?;

    // 使用 Main Client (AS Bot) 进行撤回
    let resolve = ctx.main_client.resolve_room_alias(&alias).await?;
    if let Some(room) = ctx.main_client.get_room(&resolve.room_id) {
        if let Ok(eid) = EventId::parse(&comment_id) {
            room.redact(&eid, reason.as_deref(), None).await?;
        }
    }
    Ok(())
}

// 用户编辑逻辑
pub async fn execute_user_edit(
    ctx: &AsContext,
    site_id: SiteId,
    slug: String,
    comment_id: String,
    content: String,
    user_fingerprint: String,
) -> Result<()> {
    // 1. 校验指纹并获取原始评论
    let comment_opt = ctx.db.get_comment(&comment_id).await?;
    let c = match comment_opt {
        Some(c) if c.author_fingerprint == Some(user_fingerprint) => c,
        _ => return Err(anyhow::anyhow!("Permission denied or comment not found")),
    };

    // 2. 解析房间 ID
    let alias_str = format!("#{}_{}:{}", site_id.as_str(), slug, ctx.config.server_name);
    let alias = RoomAliasId::parse(&alias_str)?;
    // 使用 main_client 解析别名即可，这很快
    let resolve = ctx.main_client.resolve_room_alias(&alias).await?;
    let room_id = resolve.room_id;

    // 3. 获取 Ghost User 的 Client (关键步骤)
    // 我们需要用发帖人的身份去编辑，而不是用 AS Bot
    let author_uid = UserId::parse(&c.author_id)?;
    let ghost_client = get_ghost_client(&ctx.config, &author_uid).await?;

    // 4. 构造标准编辑消息 (m.replace)
    let fallback_text = format!("* {}", content);
    let mut msg_content = RoomMessageEventContent::text_plain(fallback_text);

    msg_content.relates_to = Some(Relation::Replacement(Replacement::new(
        EventId::parse(&comment_id)?,
        RoomMessageEventContent::text_plain(content).into(),
    )));

    // 5. 发送消息
    // 尝试直接获取房间，如果获取不到（Ghost 可能刚初始化），则通过 ID 加入/获取
    if let Some(room) = ghost_client.get_room(&room_id) {
        room.send(msg_content).await?;
    } else {
        // Ghost Client 可能没有同步过这个房间的状态，显式 join 一下以获取 Room 对象
        let room = ghost_client.join_room_by_id(&room_id).await?;
        room.send(msg_content).await?;
    }

    Ok(())
}

// ================================================================================================
//  Incoming: 处理来自 Web Server 的事件
// ================================================================================================

pub async fn handle_incoming_message(event: OriginalRoomMessageEvent, ctx: &AsContext) -> Result<()> {
    let room_id_str = event.room_id.to_string();
    let sender_id = event.sender.to_string();

    let bot_exact = format!("@{}:{}", ctx.config.bot_localpart, ctx.config.server_name);
    let bot_prefix = format!("@{}_", ctx.config.bot_localpart);

    // 过滤 Bot 自己的消息
    if sender_id == bot_exact || sender_id.starts_with(&bot_prefix) {
        return Ok(());
    }

    // 查找房间元数据
    let (site_id, post_slug) = match ctx.db.get_room_meta(&room_id_str).await? {
        Some(meta) => meta,
        None => return Ok(()),
    };

    let content_json = serde_json::to_value(&event.content)?;

    // 时间处理
    let current_ts_millis: i64 = event.origin_server_ts.get().into();
    let current_time = chrono::DateTime::from_timestamp_millis(current_ts_millis)
        .map(|dt| dt.naive_utc())
        .unwrap_or_else(|| chrono::Utc::now().naive_utc());

    // 解析编辑
    let (target_id, final_content_json, updated_at) =
        if let Some(Relation::Replacement(ref re)) = event.content.relates_to {
            let original_id = re.event_id.to_string();
            let new_content_val =
                serde_json::to_value(&re.new_content).unwrap_or_else(|_| content_json.clone());
            (original_id, new_content_val, Some(current_time))
        } else {
            (event.event_id.to_string(), content_json, None)
        };

    // 提取评论数据
    let (mut author_name, is_guest, content, author_fingerprint, txn_id) =
        protocol::extract_comment_data(&final_content_json, &sender_id, &bot_exact);

    if content.trim().is_empty() {
        return Ok(());
    }

    // --- Profile 增强 (Hydration) ---
    let mut avatar_url = None;
    if !is_guest {
        let user_id_str = sender_id.clone();

        // 1. 查本地缓存
        let cached = ctx.db.get_cached_profile(&user_id_str).await.unwrap_or(None);
        if let Some(profile) = cached {
             if let Some(n) = profile.display_name { author_name = n; }
             avatar_url = profile.avatar_url;
        } else {
            // 2. 远程获取 (AS Token 可以获取任意用户信息)
            if let Ok(profile_resp) = ctx.main_client.get_profile(&event.sender).await {
                 let display_name = profile_resp.displayname;
                 let avatar = profile_resp.avatar_url.map(|u| u.to_string());

                 // 更新缓存
                 let _ = ctx.db.upsert_profile(&user_id_str, display_name.as_deref(), avatar.as_deref()).await;

                 if let Some(n) = display_name { author_name = n; }
                 avatar_url = avatar;
            }
        }
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

    // 修改：只序列化 content
    let raw_event = serde_json::to_string(&event.content).ok();

    ctx.db
        .upsert_comment(&room_id_str, site_id.as_str(), &post_slug, &comment, raw_event)
        .await?;

    let _ = ctx.tx_ingest.send(IngestEvent::CommentSaved {
        site_id,
        post_slug,
        comment,
    });

    Ok(())
}

pub async fn handle_incoming_redaction(event: OriginalRoomRedactionEvent, ctx: &AsContext) -> Result<()> {
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
