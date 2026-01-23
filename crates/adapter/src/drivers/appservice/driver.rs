use super::{handlers, web, AsContext};
use crate::common::matrix_utils::SpaceCache;
use crate::traits::MatrixDriver;
use crate::{AppServiceConfig, CommandEnvelope};
use anyhow::Result;
use async_trait::async_trait;
use axum::{routing::put, Router};
use domain::{AppCommand, IngestEvent};
use matrix_sdk::{
    matrix_auth::{MatrixSession, MatrixSessionTokens},
    Client, SessionMeta,
    ruma::UserId,
};
use std::net::SocketAddr;
use storage::Db;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

pub struct AppServiceDriver {
    config: AppServiceConfig,
}

impl AppServiceDriver {
    pub fn new(config: AppServiceConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl MatrixDriver for AppServiceDriver {
    async fn run(
        &self,
        db: Db,
        mut rx_cmd: mpsc::Receiver<CommandEnvelope>,
        tx_ingest: broadcast::Sender<IngestEvent>,
        cancel_token: CancellationToken,
    ) -> Result<()> {
        info!("Starting AppService Driver on port {}", self.config.listen_port);

        // 1. 初始化 Main Client (AS Bot)
        let main_client = Client::builder()
            .homeserver_url(&self.config.homeserver_url)
            .build()
            .await?;

        let main_user_id = UserId::parse(format!(
            "@{}:{}",
            self.config.bot_localpart, self.config.server_name
        ))?;

        let session = MatrixSession {
            meta: SessionMeta {
                user_id: main_user_id.clone(),
                device_id: "CUMMENTS_AS_BOT".into(),
            },
            tokens: MatrixSessionTokens {
                access_token: self.config.as_token.clone(),
                refresh_token: None,
            },
        };
        main_client.matrix_auth().restore_session(session).await?;
        info!("AS Main Bot logged in as {}", main_user_id);

        let space_cache = SpaceCache::new();

        // 2. 构建共享上下文
        let ctx = AsContext {
            db: db.clone(), // Clone db here for context
            tx_ingest,
            config: self.config.clone(),
            main_client: main_client.clone(),
        };

        // 3. 启动 Web Server
        let app = Router::new()
            .route("/transactions/:txn_id", put(web::handle_transaction))
            .with_state(ctx.clone());

        let addr = SocketAddr::from(([0, 0, 0, 0], self.config.listen_port));
        let listener = tokio::net::TcpListener::bind(addr).await?;

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                error!("AppService WebServer crashed: {}", e);
            }
        });

        // 4. 启动 Command Loop
        let cmd_handle = {
            let ctx = ctx.clone();
            let cache = space_cache;
            let cmd_cancel_token = cancel_token.clone();
            let owner_id_ref = self.config.owner_id.clone();

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
                                    handlers::execute_send(
                                        &ctx, &cache, site_id, post_slug, content, nickname, email, guest_token, reply_to, txn_id, owner_id_ref.as_ref()
                                    ).await
                                }
                                AppCommand::RedactComment { site_id, post_slug, comment_id, reason, .. } => {
                                    handlers::execute_redact(&ctx, site_id, post_slug, comment_id, reason).await
                                }
                                AppCommand::UserDeleteComment { site_id, post_slug, comment_id, user_fingerprint, .. } => {
                                    // 检查指纹
                                    match ctx.db.get_comment(&comment_id).await {
                                        Ok(Some(c)) if c.author_fingerprint == Some(user_fingerprint) => {
                                             handlers::execute_redact(&ctx, site_id, post_slug, comment_id, Some("User deleted".into())).await
                                        },
                                        Ok(Some(_)) => Err(anyhow::anyhow!("Permission denied")),
                                        _ => Err(anyhow::anyhow!("Comment not found")),
                                    }
                                }
                                AppCommand::UserEditComment { site_id, post_slug, comment_id, content, user_fingerprint, .. } => {
                                    handlers::execute_user_edit(&ctx, site_id, post_slug, comment_id, content, user_fingerprint).await
                                }
                            };

                            let _ = resp.send(result);
                        },
                        _ = cmd_cancel_token.cancelled() => break,
                    }
                }
            })
        };

        // 5. 等待停止信号
        cancel_token.cancelled().await;

        // 给一点时间让子任务退出
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), cmd_handle).await;

        Ok(())
    }
}
