mod handlers;
mod matrix_utils;

pub use matrix_utils::SpaceCache;

use domain::{protocol, AppCommand, IngestEvent};
use handlers::{handle_multitenant_send, handle_sync_event};
use matrix_sdk::{
    config::SyncSettings,
    matrix_auth::{MatrixSession, MatrixSessionTokens},
    ruma::{
        events::{
            room::message::OriginalSyncRoomMessageEvent,
            room::redaction::OriginalSyncRoomRedactionEvent,
        },
        OwnedUserId,
    },
    Client, Room, SessionMeta,
};
use std::time::Duration;
use storage::Db;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info};

pub struct MatrixConfig {
    pub homeserver_url: String,
    pub user_id: OwnedUserId,
    pub access_token: String,
}

pub async fn start(
    config: MatrixConfig,
    db: Db,
    mut rx: mpsc::Receiver<AppCommand>,
    tx_ingest: broadcast::Sender<IngestEvent>,
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

    let space_cache = SpaceCache::new();

    let sender_client = client.clone();
    let server_name_task = server_name.clone();

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
                        &server_name_task,
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
    let tx_sync = tx_ingest.clone();

    client.add_event_handler(
        move |event: OriginalSyncRoomMessageEvent, room: Room, client: Client| {
            let db = db_sync.clone();
            let bot_id = bot_id_sync.clone();
            let tx = tx_sync.clone();
            async move {
                if let Err(e) = handle_sync_event(event, room, client, db, bot_id, tx).await {
                    error!("Sync error: {:?}", e);
                }
            }
        },
    );

    let db_redact = db.clone();
    let tx_redact = tx_ingest.clone();

    client.add_event_handler(move |event: OriginalSyncRoomRedactionEvent, _: Client| {
        let db = db_redact.clone();
        let tx = tx_redact.clone();
        async move {
            if let Some(redacts_id) = event.redacts {
                let id_str = redacts_id.to_string();
                info!("Redaction detected, soft deleting: {}", id_str);

                match db.delete_comment(&id_str).await {
                    Ok(Some((site_id, post_slug))) => {
                        info!("Broadcasting deletion for {}/{}", site_id, post_slug);
                        let _ = tx.send(IngestEvent::CommentDeleted {
                            site_id,
                            post_slug,
                            comment_id: id_str,
                        });
                    }
                    Ok(None) => {}
                    Err(e) => error!("Failed to delete comment: {:?}", e),
                }
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
