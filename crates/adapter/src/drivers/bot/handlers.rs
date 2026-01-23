use crate::common::matrix_utils::{
    compute_user_fingerprint, create_and_link_room, ensure_site_space, resolve_room_alias_chain,
    SpaceCache,
};
use anyhow::Result;
use domain::{protocol, Comment, IngestEvent, SiteId};
use matrix_sdk::{
    ruma::{
        events::{
            room::message::{OriginalSyncRoomMessageEvent, Relation, RoomMessageEventContent},
            AnyMessageLikeEventContent,
            relation::Replacement,
        },
        serde::Raw,
        EventId, OwnedUserId, RoomAliasId, ServerName,
    },
    Client, Room,
};
use storage::Db;
use tokio::sync::broadcast;

// ... resolve_event_details 保持不变 ...
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
        .map(|dt| dt.naive_utc())
        .unwrap_or_else(|| chrono::Utc::now().naive_utc());

    if let Some(Relation::Replacement(ref re)) = event.content.relates_to {
        let original_id = re.event_id.to_string();
        let new_content_val = serde_json::to_value(&re.new_content).unwrap_or(content_json.clone());
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
    // 1. 解析别名
    let alias_str = match resolve_room_alias_chain(&room, &client).await {
        Some(a) => a,
        None => return Ok(()),
    };

    let localpart = alias_str
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_start_matches('#');
    let (site_id, post_slug) = match protocol::parse_room_alias(localpart) {
        Some(res) => res,
        None => return Ok(()),
    };

    let content_json = serde_json::to_value(&event.content)?;
    let (target_id, final_content_json, current_time, updated_at) =
        resolve_event_details(&event, content_json);

    let sender_id = event.sender.to_string();

    let (mut author_name, is_guest, content, author_fingerprint, txn_id) =
        protocol::extract_comment_data(&final_content_json, &sender_id, &bot_id);

    if content.trim().is_empty() {
        return Ok(());
    }

    // Profile Fetching
    let mut avatar_url = None;
    if !is_guest {
        let user_id_str = sender_id.clone();
        let cached = db.get_cached_profile(&user_id_str).await.unwrap_or(None);

        if let Some(profile) = cached {
            if let Some(name) = profile.display_name { author_name = name; }
            avatar_url = profile.avatar_url;
        } else {
            if let Ok(Some(profile)) = room.get_member(&event.sender).await.map(|m| m.map(|x| x.clone())) {
                let display_name = profile.display_name().map(|s| s.to_string());
                let avatar = profile.avatar_url().map(|s| s.to_string());
                let _ = db.upsert_profile(&user_id_str, display_name.as_deref(), avatar.as_deref()).await;
                if let Some(n) = display_name { author_name = n; }
                avatar_url = avatar;
            } else if let Ok(profile_resp) = client.get_profile(&event.sender).await {
                 let display_name = profile_resp.displayname;
                 let avatar = profile_resp.avatar_url.map(|u| u.to_string());
                 let _ = db.upsert_profile(&user_id_str, display_name.as_deref(), avatar.as_deref()).await;
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

    let room_id = room.room_id().as_str();

    // 修改：只序列化 content 以修复编译错误
    let raw_event = serde_json::to_string(&event.content).ok();

    db.upsert_comment(room_id, site_id.as_str(), &post_slug, &comment, raw_event)
        .await?;

    let _ = tx.send(IngestEvent::CommentSaved {
        site_id,
        post_slug,
        comment,
    });

    Ok(())
}

// 确保这些函数是 pub 的
pub async fn execute_send(
    client: &Client,
    server_name: &ServerName,
    db: &Db,
    cache: &SpaceCache,
    salt: &str,
    owner_id: Option<&OwnedUserId>,
    site_id: SiteId,
    post_slug: String,
    content: String,
    nickname: String,
    email: Option<String>,
    guest_token: String,
    reply_to: Option<String>,
    txn_id: Option<String>,
) -> Result<()> {
    let fingerprint = compute_user_fingerprint(email.as_deref(), &guest_token, salt);
    let event_json = protocol::build_outbound_event(&nickname, &content, Some(fingerprint), txn_id);

    let space_id = ensure_site_space(client, server_name, cache, &site_id).await?;
    let full_alias = format!("#{}_{}:{}", site_id.as_str(), post_slug, server_name);
    let room_alias = RoomAliasId::parse(&full_alias)?;

    let room = match client.resolve_room_alias(&room_alias).await {
        Ok(resp) => match client.get_room(&resp.room_id) {
            Some(r) => r,
            None => client.join_room_by_id(&resp.room_id).await?,
        },
        Err(_) => {
            create_and_link_room(client, server_name, &space_id, &site_id, &post_slug, owner_id)
                .await?
        }
    };

    db.ensure_room(room.room_id().as_str(), site_id.as_str(), &post_slug)
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
        }
    }

    let raw_content: Raw<AnyMessageLikeEventContent> = serde_json::from_value(final_json)?;
    room.send_raw("m.room.message", raw_content).await?;
    Ok(())
}

pub async fn execute_redact(
    client: &Client,
    server_name: &ServerName,
    site_id: SiteId,
    slug: String,
    comment_id: String,
    reason: Option<String>,
) -> Result<()> {
    let alias_str = format!("#{}_{}:{}", site_id.as_str(), slug, server_name);
    let alias = RoomAliasId::parse(&alias_str)?;

    let room_id = client.resolve_room_alias(&alias).await?.room_id;
    let room = client
        .get_room(&room_id)
        .ok_or_else(|| anyhow::anyhow!("Bot not in room"))?;

    let eid = EventId::parse(&comment_id)?;
    room.redact(&eid, reason.as_deref(), None).await?;

    Ok(())
}

pub async fn execute_user_delete(
    client: &Client,
    server_name: &ServerName,
    db: &Db,
    site_id: SiteId,
    slug: String,
    comment_id: String,
    user_fingerprint: String,
) -> Result<()> {
    let comment = db.get_comment(&comment_id).await?;
    match comment {
        Some(c) if c.author_fingerprint == Some(user_fingerprint) => {
             execute_redact(
                client,
                server_name,
                site_id,
                slug,
                comment_id,
                Some("User deleted their comment".to_string()),
            )
            .await
        }
        Some(_) => Err(anyhow::anyhow!("Permission denied: Fingerprint mismatch")),
        None => Err(anyhow::anyhow!("Comment not found")),
    }
}

pub async fn execute_user_edit(
    client: &Client,
    server_name: &ServerName,
    db: &Db,
    site_id: SiteId,
    slug: String,
    comment_id: String,
    new_content: String,
    user_fingerprint: String,
) -> Result<()> {
    // 1. 权限校验 (保持不变)
    let comment_opt = db.get_comment(&comment_id).await?;
    match comment_opt {
        // 如果指纹匹配，什么都不做，继续执行
        Some(c) if c.author_fingerprint == Some(user_fingerprint) => {},
        // 否则报错返回
        _ => return Err(anyhow::anyhow!("Permission denied or comment not found")),
    };

    // 2. 获取房间 (保持不变)
    let alias_str = format!("#{}_{}:{}", site_id.as_str(), slug, server_name);
    let alias = RoomAliasId::parse(&alias_str)?;
    let room_id = client.resolve_room_alias(&alias).await?.room_id;
    let room = client.get_room(&room_id).ok_or_else(|| anyhow::anyhow!("Bot not in room"))?;

    // [重写发送逻辑]
    let fallback_text = format!("* {}", new_content);
    let mut content = RoomMessageEventContent::text_plain(fallback_text);

    // 构造 m.replace 关系
    content.relates_to = Some(Relation::Replacement(Replacement::new(
        EventId::parse(&comment_id)?,
        RoomMessageEventContent::text_plain(new_content).into(),
    )));

    room.send(content).await?;
    Ok(())
}
