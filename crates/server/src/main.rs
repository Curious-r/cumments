mod config;
mod http;
mod pow;
mod state;

use anyhow::Context;
use dotenvy::dotenv;
use tokio::sync::{broadcast, mpsc};
use tracing::info;

use config::Settings;
use http::router::build_router;
use pow::PowGuard;
use state::AppState;
use storage::Db;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    tracing_subscriber::fmt::init();

    let settings = Settings::new().context("Failed to load configuration")?;

    let db = Db::new(&settings.database.url).await?;

    let (tx_cmd, rx_cmd) = mpsc::channel(100);
    let (tx_ingest, _rx_ingest) = broadcast::channel(100);

    let matrix_config = match settings.matrix {
        config::MatrixSettings::Bot {
            homeserver_url,
            user,
            token,
        } => {
            use matrix_sdk::ruma::UserId;
            let user_id = UserId::parse(&user)
                .map_err(|e| anyhow::anyhow!("Invalid Matrix User ID: {}", e))?;

            adapter::MatrixConfig::Bot(adapter::BotConfig {
                homeserver_url,
                user_id,
                access_token: token,
                identity_salt: settings.security.identity_salt.clone(),
            })
        }
        config::MatrixSettings::AppService {
            homeserver_url,
            server_name,
            as_token,
            hs_token,
            bot_localpart,
            listen_port,
        } => adapter::MatrixConfig::AppService(adapter::AppServiceConfig {
            homeserver_url,
            server_name,
            as_token,
            hs_token,
            bot_localpart,
            listen_port,
            identity_salt: settings.security.identity_salt.clone(),
        }),
    };

    let db_for_worker = db.clone();
    let tx_ingest_for_worker = tx_ingest.clone();

    tokio::spawn(async move {
        if let Err(e) =
            adapter::start(matrix_config, db_for_worker, rx_cmd, tx_ingest_for_worker).await
        {
            tracing::error!("Matrix worker crashed: {:?}", e);
        }
    });

    let state = AppState {
        db,
        sender: tx_cmd,
        tx_ingest,
        pow: PowGuard::new(),
    };

    let app = build_router(state, &settings.server.cors_origins);

    let addr = format!("{}:{}", settings.server.host, settings.server.port);
    info!("Server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to address: {}", addr))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received Ctrl+C, shutting down gracefully...");
        },
        _ = terminate => {
            info!("Received SIGTERM, shutting down gracefully...");
        },
    }
}
