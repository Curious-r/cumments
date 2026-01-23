mod config;
mod http;
mod pow;
mod state;

use anyhow::Context;
use dotenvy::dotenv;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::info;
use matrix_sdk::ruma::UserId;

use config::Settings;
use http::router::build_router;
use pow::PowGuard;
use state::AppState;
use storage::Db;
use adapter::CommandEnvelope;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();

    // 设置日志系统的默认级别
    // 逻辑：优先读取 RUST_LOG 环境变量；如果没有，则使用默认值 "info,sqlx=warn"
    // "sqlx=warn" 是为了防止 sqlx 输出海量的查询日志，淹没业务信息
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "server=info,adapter=info,storage=info,tower_http=debug,sqlx=warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let settings = Settings::new().context("Failed to load configuration")?;

    let db = Db::new(&settings.database.url).await?;

    // Channel 类型改为 CommandEnvelope
    let (tx_cmd, rx_cmd) = mpsc::channel::<CommandEnvelope>(100);
    let (tx_ingest, _rx_ingest) = broadcast::channel(100);

    let matrix_config = match settings.matrix {
        config::MatrixSettings::Bot {
            homeserver_url,
            user,
            token,
            device_id,
            owner_id, // 读取配置
        } => {
            let user_id = UserId::parse(&user)
                .map_err(|e| anyhow::anyhow!("Invalid Matrix User ID: {}", e))?;

            let owner_user_id = if let Some(o) = owner_id {
                Some(UserId::parse(&o).map_err(|e| anyhow::anyhow!("Invalid Owner ID: {}", e))?)
            } else {
                None
            };

            adapter::MatrixConfig::Bot(adapter::BotConfig {
                homeserver_url,
                user_id,
                access_token: token,
                identity_salt: settings.security.identity_salt.clone(),
                device_id: device_id.unwrap_or_else(|| "CUMMENTS_BOT_V4".to_string()),
                owner_id: owner_user_id, // 传递 owner_id
            })
        }
        config::MatrixSettings::AppService {
            homeserver_url,
            server_name,
            as_token,
            hs_token,
            bot_localpart,
            listen_port,
            owner_id,
        } => {
             let owner_user_id = if let Some(o) = owner_id {
                Some(UserId::parse(&o).map_err(|e| anyhow::anyhow!("Invalid Owner ID: {}", e))?)
            } else {
                None
            };

            adapter::MatrixConfig::AppService(adapter::AppServiceConfig {
                homeserver_url,
                server_name,
                as_token,
                hs_token,
                bot_localpart,
                listen_port,
                identity_salt: settings.security.identity_salt.clone(),
                owner_id: owner_user_id,
            })
        },
    };

    let cancel_token = CancellationToken::new();
    let matrix_cancel_token = cancel_token.clone();
    let server_cancel_token = cancel_token.clone();
    let db_for_worker = db.clone();
    let tx_ingest_for_worker = tx_ingest.clone();

    let matrix_task = tokio::spawn(async move {
        if let Err(e) = adapter::start_with_cancel_token(
            matrix_config,
            db_for_worker,
            rx_cmd,
            tx_ingest_for_worker,
            matrix_cancel_token,
        )
        .await
        {
            tracing::error!("Matrix worker crashed: {:?}", e);
        }
    });

    let state = AppState {
        db,
        sender: tx_cmd,
        tx_ingest,
        pow: PowGuard::new(settings.security.pow_secret.clone()),
        admin_token: settings.security.admin_token.clone(),
        server_name: settings.server.public_server_name.clone(), // 传递 Server Name
    };

    let app = build_router(state, &settings.server.cors_origins);

    let addr = format!("{}:{}", settings.server.host, settings.server.port);
    info!("Server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to address: {}", addr))?;

    // 启动 Server 任务
    // 修改 Axum 的退出条件：不再监听 OS 信号，而是等待 server_cancel_token 被取消
    let server_task = tokio::spawn(async move {
        let shutdown_future = async move {
            server_cancel_token.cancelled().await;
        };

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_future) // 这里改了
            .await
    });

    // 主线程监听 OS 信号
    // 使用一个新的函数来统一处理 Ctrl+C 和 SIGTERM
    match wait_for_os_signal().await {
        Ok(()) => info!("Received OS shutdown signal"),
        Err(err) => info!("Error listening for signal: {}", err),
    }

    info!("Cancelling all operations...");

    // 触发 Token，通知 Matrix Adapter 和 Axum Server 停止
    cancel_token.cancel();

    // 等待子任务退出
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), server_task).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), matrix_task).await;

    info!("Graceful shutdown completed");
    Ok(())
}

// 重写信号监听函数
// 这个函数只负责监听系统信号，不负责业务逻辑
async fn wait_for_os_signal() -> std::io::Result<()> {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
            Ok(())
        } else {
            std::future::pending::<std::io::Result<()>>().await
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<std::io::Result<()>>();

    tokio::select! {
        res = ctrl_c => res,
        _ = terminate => Ok(()),
    }
}
