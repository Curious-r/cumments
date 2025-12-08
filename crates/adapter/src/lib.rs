use domain::{protocol, AppCommand, Comment, SiteId};
use matrix_sdk::{
    config::SyncSettings,
    matrix_auth::{MatrixSession, MatrixSessionTokens},
    ruma::{
        api::client::room::create_room::v3::Request as CreateRoomRequest,
        api::client::room::create_room::v3::RoomPreset,
        events::{
            room::message::{OriginalSyncRoomMessageEvent, Relation},
            room::redaction::OriginalSyncRoomRedactionEvent,
            space::child::SpaceChildEventContent,
            AnyMessageLikeEventContent,
        },
        room::RoomType,
        serde::Raw,
        EventId, OwnedRoomId, OwnedUserId, RoomAliasId, ServerName,
    },
    Client, Room, SessionMeta,
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use storage::Db;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info};

pub struct MatrixConfig {
    pub homeserver_url: String,
    pub user_id: OwnedUserId,
    pub access_token: String,
}

struct SpaceCache {
    inner: Arc<RwLock<HashMap<String, OwnedRoomId>>>,
}

pub async fn start(
    config: MatrixConfig,
    db: Db,
    mut rx: mpsc::Receiver<AppCommand>,
) -> anyhow::Result<()> {
    let server_name = config.user_id.server_name().to_owned();

    let client = Client::builder()
        .homeserver_url(&config.homeserver_url)
        .build()
        .await?;

    let session = MatrixSession {
        meta: SessionMeta {
            user_id: config.user_id.clone(),
            device_id: "CUMMENTS_BOT_V4".into(),
        },
        tokens: MatrixSessionTokens {
            access_token: config.access_token.clone(),
            refresh_token: None,
        },
    };

    client.matrix_auth().restore_session(session).await?;

    info!("Matrix Client logged in as {}", config.user_id);

    let my_bot_id = client.user_id().unwrap().to_string();
    let space_cache = SpaceCache {
        inner: Arc::new(RwLock::new(HashMap::new())),
    };

    // --- Write Task ---
    let sender_client = client.clone();
    let server_name_for_task = server_name.clone();

    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                AppCommand::SendComment {
                    site_id,
                    post_slug,
                    content,
                    nickname,
                    reply_to,
                } => {
                    let event_json = protocol::build_outbound_event(&nickname, &content);

                    if let Err(e) = handle_multitenant_send(
                        &sender_client,
                        &server_name_for_task,
                        &space_cache,
                        &site_id,
                        &post_slug,
                        event_json,
                        reply_to,
                    )
                    .await
                    {
                        error!("Send failed: {:?}", e);
                    }
                }
            }
        }
    });

    // --- Event Handlers ---
    let db_sync = db.clone();
    let bot_id_sync = my_bot_id.clone();
    client.add_event_handler(
        move |event: OriginalSyncRoomMessageEvent, room: Room, client: Client| {
            let db = db_sync.clone();
            let bot_id = bot_id_sync.clone();
            async move {
                if let Err(e) = handle_sync_event(event, room, client, db, bot_id).await {
                    error!("Sync error: {:?}", e);
                }
            }
        },
    );

    let db_redact = db.clone();
    client.add_event_handler(move |event: OriginalSyncRoomRedactionEvent, _: Client| {
        let db = db_redact.clone();
        async move {
            if let Some(redacts_id) = event.redacts {
                info!("Redaction detected, soft deleting: {}", redacts_id);
                let _ = db.delete_comment(&redacts_id.to_string()).await;
            }
        }
    });

    // --- Manual Sync Loop ---
    info!("Starting Matrix Sync Loop...");
    let mut sync_token = db.get_sync_token().await?;
    if let Some(ref t) = sync_token {
        info!("Resuming sync from token: {}", t);
    }

    loop {
        let mut settings = SyncSettings::default().timeout(Duration::from_secs(30));
        if let Some(ref token) = sync_token {
            settings = settings.token(token);
        }

        match client.sync_once(settings).await {
            Ok(response) => {
                let next_batch = response.next_batch;
                if Some(&next_batch) != sync_token.as_ref() {
                    if let Err(e) = db.save_sync_token(&next_batch).await {
                        error!("CRITICAL: Failed to save sync token: {:?}", e);
                    } else {
                        sync_token = Some(next_batch);
                    }
                }
            }
            Err(e) => {
                error!("Matrix sync failed: {:?}. Retrying in 5s...", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn handle_sync_event(
    event: OriginalSyncRoomMessageEvent,
    room: Room,
    _client: Client,
    db: Db,
    bot_id: String,
) -> anyhow::Result<()> {
    let content_json = serde_json::to_value(&event.content)?;

    let room_alias_str = if let Some(c) = room.canonical_alias() {
        c.to_string()
    } else if let Some(alt) = room.alt_aliases().first() {
        alt.to_string()
    } else {
        return Ok(());
    };

    let localpart = room_alias_str
        .split(':')
        .next()
        .unwrap_or("")
        .trim_start_matches('#');

    let (site_id, post_slug) = match protocol::parse_room_alias(localpart) {
        Some(res) => res,
        None => return Ok(()),
    };

    let sender_id = event.sender.to_string();

    let (author_name, is_guest, content) =
        protocol::extract_comment_data(&content_json, &sender_id, &bot_id);

    if content.trim().is_empty() {
        return Ok(());
    }

    let reply_to = if let Some(Relation::Reply { in_reply_to }) = event.content.relates_to {
        Some(in_reply_to.event_id.to_string())
    } else {
        None
    };

    let ts_millis: i64 = event.origin_server_ts.get().into();
    let created_at = chrono::DateTime::from_timestamp_millis(ts_millis)
        .unwrap_or_default()
        .naive_utc();

    let comment = Comment {
        id: event.event_id.to_string(),
        site_id,
        post_slug,
        author_id: sender_id,
        author_name,
        is_guest,
        is_redacted: false,
        content,
        created_at,
        reply_to,
    };

    db.upsert_comment(&comment).await?;
    info!("Comment synced: {} -> {}", comment.id, comment.content);
    Ok(())
}

async fn handle_multitenant_send(
    client: &Client,
    server_name: &ServerName,
    cache: &SpaceCache,
    site_id: &SiteId,
    slug: &str,
    event_json: serde_json::Value,
    reply_to: Option<String>,
) -> anyhow::Result<()> {
    let space_id = ensure_site_space(client, server_name, cache, site_id).await?;

    let full_alias = format!("#{}_{}:{}", site_id.as_str(), slug, server_name);
    let room_alias = RoomAliasId::parse(&full_alias)?;

    let room = match client.resolve_room_alias(&room_alias).await {
        Ok(resp) => client
            .get_room(&resp.room_id)
            .unwrap_or(client.join_room_by_id(&resp.room_id).await?),
        Err(_) => {
            let alias_local = format!("{}_{}", site_id.as_str(), slug);
            let mut req = CreateRoomRequest::new();
            req.room_alias_name = Some(alias_local);
            req.name = Some(format!("Comments for {}", slug));
            req.preset = Some(RoomPreset::PublicChat);

            let room = client.create_room(req).await?;

            if let Some(space_room) = client.get_room(&space_id) {
                let server_name_owned = server_name.to_owned();
                let child = SpaceChildEventContent::new(vec![server_name_owned]);
                space_room
                    .send_state_event_for_key(room.room_id(), child)
                    .await?;
            }
            room
        }
    };

    let mut final_json = event_json;

    if let Some(parent_id_str) = reply_to {
        if let Ok(_) = EventId::parse(&parent_id_str) {
            if let Some(obj) = final_json.as_object_mut() {
                obj.insert(
                    "m.relates_to".to_string(),
                    serde_json::json!({
                        "m.in_reply_to": {
                            "event_id": parent_id_str
                        }
                    }),
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

async fn ensure_site_space(
    client: &Client,
    server_name: &ServerName,
    cache: &SpaceCache,
    site_id: &SiteId,
) -> anyhow::Result<OwnedRoomId> {
    let site_id_str = site_id.as_str();

    {
        if let Some(id) = cache.inner.read().await.get(site_id_str) {
            return Ok(id.clone());
        }
    }

    let alias_local = format!("cumments_{}", site_id_str);
    let full_alias = format!("#{}:{}", alias_local, server_name);
    let alias = RoomAliasId::parse(&full_alias)?;

    let room_id = match client.resolve_room_alias(&alias).await {
        Ok(resp) => resp.room_id.to_owned(),
        Err(_) => {
            let mut cc =
                matrix_sdk::ruma::api::client::room::create_room::v3::CreationContent::new();
            cc.room_type = Some(RoomType::Space);

            let mut req = CreateRoomRequest::new();
            req.room_alias_name = Some(alias_local);
            req.name = Some(site_id_str.to_string());
            req.creation_content = Some(Raw::new(&cc)?);
            req.preset = Some(RoomPreset::PublicChat);

            let r = client.create_room(req).await?;
            r.room_id().to_owned()
        }
    };

    {
        cache
            .inner
            .write()
            .await
            .insert(site_id_str.to_string(), room_id.clone());
    }
    Ok(room_id)
}
