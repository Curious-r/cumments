mod config;
mod http;
mod pow;
mod state;

use anyhow::Context;
use clap::Parser;
use dotenvy::dotenv;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::info;

use adapter::CommandEnvelope;
use config::Settings;
use http::router::build_router;
use pow::PowGuard;
use state::AppState;
use storage::Db;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, value_name = "FILE")]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "server=info,adapter=info,storage=info,tower_http=debug,sqlx=warn".into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::parse();

    let settings = Settings::new(args.config).context("Failed to load configuration")?;

    let db = Db::new(&settings.database.url).await?;

    let (tx_cmd, rx_cmd) = mpsc::channel::<CommandEnvelope>(100);
    let (tx_ingest, _rx_ingest) = broadcast::channel(100);

    let matrix_config = match settings.matrix {
        config::MatrixSettings::Bot {
            homeserver_url,
            user,
            token,
            device_id,
            owner_id,
        } => adapter::MatrixConfig::Bot(adapter::BotConfig {
            homeserver_url,
            user_id: user,
            access_token: token,
            identity_salt: settings.security.identity_salt.clone(),
            device_id: device_id.unwrap_or_else(|| "CUMMENTS_BOT_V4".to_string()),
            owner_id,
        }),
        config::MatrixSettings::AppService {
            homeserver_url,
            server_name,
            as_token,
            hs_token,
            bot_localpart,
            listen_port,
            owner_id,
        } => adapter::MatrixConfig::AppService(adapter::AppServiceConfig {
            homeserver_url,
            server_name,
            as_token,
            hs_token,
            bot_localpart,
            listen_port,
            identity_salt: settings.security.identity_salt.clone(),
            owner_id,
        }),
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

    let server_task = tokio::spawn(async move {
        let shutdown_future = async move {
            server_cancel_token.cancelled().await;
        };

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_future)
            .await
    });

    match wait_for_os_signal().await {
        Ok(()) => info!("Received OS shutdown signal"),
        Err(err) => info!("Error listening for signal: {}", err),
    }

    info!("Cancelling all operations...");

    cancel_token.cancel();

    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), server_task).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), matrix_task).await;

    info!("Graceful shutdown completed");
    Ok(())
}

async fn wait_for_os_signal() -> std::io::Result<()> {
    let ctrl_c = async { tokio::signal::ctrl_c().await };

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
