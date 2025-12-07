use domain::{Comment, MatrixCommand};
use matrix_sdk::{
    config::SyncSettings,
    matrix_auth::{MatrixSession, MatrixSessionTokens},
    ruma::{
        api::client::room::create_room::v3::Request as CreateRoomRequest,
        api::client::room::create_room::v3::RoomPreset,
        events::{
            room::message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent},
            room::redaction::OriginalSyncRoomRedactionEvent,
            space::child::SpaceChildEventContent,
        },
        room::RoomType,
        serde::Raw,
        OwnedRoomId, OwnedUserId, RoomAliasId, ServerName,
    },
    Client, Room, SessionMeta,
};
use std::{collections::HashMap, sync::Arc};
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
    mut rx: mpsc::Receiver<MatrixCommand>,
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
                MatrixCommand::SendComment {
                    site_id,
                    post_slug,
                    content,
                } => {
                    if let Err(e) = handle_multitenant_send(
                        &sender_client,
                        &server_name_for_task,
                        &space_cache,
                        &site_id,
                        &post_slug,
                        &content,
                    )
                    .await
                    {
                        error!("Send failed: {:?}", e);
                    }
                }
            }
        }
    });

    // --- Sync Task (Messages) ---
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

    // --- Sync Task (Redactions) ---
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

    let token = db.get_sync_token().await?;
    let mut settings = SyncSettings::default();
    if let Some(t) = token {
        settings = settings.token(t);
    }

    info!("Starting Matrix Sync...");
    client.sync(settings).await?;
    Ok(())
}

async fn handle_sync_event(
    event: OriginalSyncRoomMessageEvent,
    room: Room,
    _client: Client,
    db: Db,
    bot_id: String,
) -> anyhow::Result<()> {
    tracing::debug!("Handling sync event: {}", event.event_id);

    let content_text = match event.content.msgtype {
        MessageType::Text(t) => t.body,
        _ => return Ok(()),
    };

    let room_alias_str = if let Some(c) = room.canonical_alias() {
        c.to_string()
    } else if let Some(alt) = room.alt_aliases().first() {
        alt.to_string()
    } else {
        tracing::warn!("Room {} has no aliases, skipping message.", room.room_id());
        return Ok(());
    };

    let localpart = room_alias_str
        .split(':')
        .next()
        .unwrap_or("")
        .trim_start_matches('#');

    let (site_id, post_slug) = if let Some((s, p)) = localpart.split_once('_') {
        (s.to_string(), p.to_string())
    } else {
        return Ok(());
    };

    let sender_id = event.sender.to_string();
    let (author_name, is_guest, content) = if sender_id == bot_id {
        if let Some((nick, text)) = parse_guest_content(&content_text) {
            (nick, true, text)
        } else {
            ("Bot".to_string(), false, content_text)
        }
    } else {
        (sender_id.clone(), false, content_text)
    };

    let is_redacted = false;

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
        is_redacted,
        content,
        created_at,
        reply_to: None,
    };

    db.upsert_comment(&comment).await?;
    tracing::info!("Comment synced: {} -> {}", comment.id, comment.content);
    Ok(())
}

fn parse_guest_content(body: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = body.splitn(2, " (Guest): ").collect();
    if parts.len() == 2 {
        let nick = parts[0]
            .trim_start_matches("**")
            .trim_end_matches("**")
            .to_string();
        Some((nick, parts[1].to_string()))
    } else {
        None
    }
}

async fn handle_multitenant_send(
    client: &Client,
    server_name: &ServerName,
    cache: &SpaceCache,
    site_id: &str,
    slug: &str,
    content: &str,
) -> anyhow::Result<()> {
    let space_id = ensure_site_space(client, server_name, cache, site_id).await?;

    let full_alias = format!("#{}_{}:{}", site_id, slug, server_name);
    let room_alias = RoomAliasId::parse(&full_alias)?;

    let room = match client.resolve_room_alias(&room_alias).await {
        Ok(resp) => client
            .get_room(&resp.room_id)
            .unwrap_or(client.join_room_by_id(&resp.room_id).await?),
        Err(_) => {
            let alias_local = format!("{}_{}", site_id, slug);
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

    room.send(RoomMessageEventContent::text_markdown(content))
        .await?;
    Ok(())
}

async fn ensure_site_space(
    client: &Client,
    server_name: &ServerName,
    cache: &SpaceCache,
    site_id: &str,
) -> anyhow::Result<OwnedRoomId> {
    {
        if let Some(id) = cache.inner.read().await.get(site_id) {
            return Ok(id.clone());
        }
    }

    let alias_local = format!("cumments_{}", site_id);
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
            req.name = Some(site_id.to_string());
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
            .insert(site_id.to_string(), room_id.clone());
    }
    Ok(room_id)
}
