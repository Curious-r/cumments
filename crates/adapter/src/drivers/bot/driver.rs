use anyhow::Result;
use async_trait::async_trait;
use domain::{AppCommand, IngestEvent};
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
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

// 确保引用路径正确
use super::handlers::{
    execute_redact, execute_send, execute_user_delete, execute_user_edit, handle_sync_event,
};
use crate::common::matrix_utils::SpaceCache;
use crate::traits::MatrixDriver;
use crate::CommandEnvelope;

#[derive(Clone)]
pub struct BotConfig {
    pub homeserver_url: String,
    pub user_id: OwnedUserId,
    pub access_token: String,
    pub identity_salt: String,
    pub device_id: String,
    pub owner_id: Option<OwnedUserId>,
}

pub struct BotDriver {
    config: BotConfig,
}

impl BotDriver {
    pub fn new(config: BotConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl MatrixDriver for BotDriver {
    async fn run(
        &self,
        db: Db,
        mut rx_cmd: mpsc::Receiver<CommandEnvelope>,
        tx_ingest: broadcast::Sender<IngestEvent>,
        cancel_token: CancellationToken,
    ) -> Result<()> {
        // --- 1. Client 初始化 ---
        let client = Client::builder()
            .homeserver_url(&self.config.homeserver_url)
            .build()
            .await?;

        let session = MatrixSession {
            meta: SessionMeta {
                user_id: self.config.user_id.clone(),
                device_id: self.config.device_id.clone().into(),
            },
            tokens: MatrixSessionTokens {
                access_token: self.config.access_token.clone(),
                refresh_token: None,
            },
        };

        client.matrix_auth().restore_session(session).await?;
        info!("Matrix Client logged in as {}", self.config.user_id);

        let my_bot_id = self.config.user_id.to_string();
        let space_cache = SpaceCache::new();

        // 补回丢失的变量克隆
        let db_for_cmd = db.clone();
        let db_for_sync = db.clone();
        let db_for_redact = db.clone();
        let db_for_main_sync = db.clone();

        // --- 2. 任务：指令处理 ---
        let cmd_handle = {
            let client = client.clone();
            let db = db_for_cmd; // Use clone
            let config = self.config.clone();
            let server_name = config.user_id.server_name().to_owned();
            let cache = space_cache;
            let cmd_cancel_token = cancel_token.clone();

            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        cmd_opt = rx_cmd.recv() => {
                            let envelope = match cmd_opt {
                                Some(e) => e,
                                None => break,
                            };

                            let CommandEnvelope { cmd, resp } = envelope;

                            let result = match cmd {
                                AppCommand::SendComment {
                                    site_id, post_slug, content, nickname, email, guest_token, reply_to, txn_id
                                } => {
                                    execute_send(
                                        &client, &server_name, &db, &cache,
                                        &config.identity_salt, config.owner_id.as_ref(),
                                        site_id, post_slug, content, nickname, email, guest_token, reply_to, txn_id
                                    ).await
                                }
                                AppCommand::RedactComment { site_id, post_slug, comment_id, reason, .. } => {
                                    execute_redact(
                                        &client, &server_name, site_id, post_slug, comment_id, reason
                                    ).await
                                }
                                AppCommand::UserDeleteComment { site_id, post_slug, comment_id, user_fingerprint, .. } => {
                                    execute_user_delete(
                                        &client, &server_name, &db, site_id, post_slug, comment_id, user_fingerprint
                                    ).await
                                }
                                AppCommand::UserEditComment { site_id, post_slug, comment_id, content, user_fingerprint, .. } => {
                                    execute_user_edit(
                                        &client, &server_name, &db, site_id, post_slug, comment_id, content, user_fingerprint
                                    ).await
                                }
                            };

                            if let Err(e) = result {
                                error!("Command execution failed: {:?}", e);
                                let _ = resp.send(Err(e));
                            } else {
                                let _ = resp.send(Ok(()));
                            }
                        },
                        _ = cmd_cancel_token.cancelled() => break,
                    }
                }
            })
        };

        // --- 3. 任务：Sync Loop & Event Handlers ---

        let db_sync = db_for_sync; // Use clone
        let bot_id_sync = my_bot_id.clone();
        let tx_sync = tx_ingest.clone();

        client.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, room: Room, c: Client| {
            let db = db_sync.clone();
            let bot_id = bot_id_sync.clone();
            let tx = tx_sync.clone();
            async move {
                if let Err(e) = handle_sync_event(ev, room, c, db, bot_id, tx).await {
                    error!("Sync error: {:?}", e);
                }
            }
        });

        let db_redact = db_for_redact; // Use clone
        let tx_redact = tx_ingest.clone();

        client.add_event_handler(move |ev: OriginalSyncRoomRedactionEvent, _: Client| {
            let db = db_redact.clone();
            let tx = tx_redact.clone();
            async move {
                if let Some(redacts_id) = ev.redacts {
                    let id_str = redacts_id.to_string();
                    if let Ok(Some((site_id, slug))) = db.delete_comment(&id_str).await {
                         let _ = tx.send(IngestEvent::CommentDeleted {
                            site_id, post_slug: slug, comment_id: id_str
                        });
                    }
                }
            }
        });

        // 启动 Sync Loop
        info!("Starting Matrix Sync Loop...");
        let mut sync_token = db_for_main_sync.get_sync_token().await?; // Use clone
        if let Some(ref t) = sync_token {
            info!("Resuming sync from token: {}", t);
        }

        let sync_cancel_token = cancel_token.clone();
        let sync_client = client.clone();
        let db_sync_save = db_for_main_sync.clone(); // Use clone

        let sync_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    sync_result = async {
                        let mut settings = SyncSettings::default().timeout(Duration::from_secs(30));
                        if let Some(ref token) = sync_token {
                            settings = settings.token(token);
                        }
                        sync_client.sync_once(settings).await
                    } => {
                        match sync_result {
                            Ok(response) => {
                                let next_batch = response.next_batch;
                                if Some(&next_batch) != sync_token.as_ref() {
                                    if let Err(e) = db_sync_save.save_sync_token(&next_batch).await {
                                        error!("CRITICAL: Failed to save sync token: {:?}", e);
                                    } else {
                                        sync_token = Some(next_batch);
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Matrix sync failed: {:?}. Retrying...", e);
                                if sync_cancel_token.is_cancelled() { break; }
                                tokio::time::sleep(Duration::from_secs(5)).await;
                            }
                        }
                    },
                    _ = sync_cancel_token.cancelled() => break,
                }
            }
        });

        // --- 4. 优雅退出 ---
        cancel_token.cancelled().await;
        let _ = tokio::join!(cmd_handle, sync_handle);
        Ok(())
    }
}
