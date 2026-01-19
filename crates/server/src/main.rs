mod app_state;
mod handlers;
mod pow;

use anyhow::Context;
use axum::{
    http::Method,
    routing::{get, post},
    Router,
};
use dotenvy::dotenv;
use tokio::sync::{broadcast, mpsc};
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

use app_state::{AppConfig, AppState};
use handlers::{get_challenge, list_comments, post_comment, sse_handler};
use pow::PowGuard;
use storage::Db;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    tracing_subscriber::fmt::init();

    let config = AppConfig::from_env()?;

    let db = Db::new(&config.db_url).await?;

    // Server -> Adapter
    let (tx_cmd, rx_cmd) = mpsc::channel(100);
    // Adapter -> Server -> SSE
    let (tx_ingest, _rx_ingest) = broadcast::channel(100);

    let db_for_worker = db.clone();
    let tx_ingest_for_worker = tx_ingest.clone();

    tokio::spawn(async move {
        if let Err(e) =
            adapter::start(config.matrix, db_for_worker, rx_cmd, tx_ingest_for_worker).await
        {
            tracing::error!("Matrix worker crashed: {:?}", e);
        }
    });

    let pow = PowGuard::new();
    let state = AppState {
        db,
        sender: tx_cmd,
        tx_ingest,
        pow,
    };

    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_origin(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/api/:site_id/comments/:slug", get(list_comments))
        .route("/api/:site_id/comments", post(post_comment))
        .route("/api/:site_id/comments/:slug/sse", get(sse_handler))
        .route("/api/challenge", get(get_challenge))
        .layer(cors)
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port);
    info!("Server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to address: {}", addr))?;

    axum::serve(listener, app).await?;
    Ok(())
}
