use anyhow::Result;
use cumments_core::site_service::SiteService;
use matrix_sdk::{
    Client, SessionMeta, authentication::SessionTokens, authentication::matrix::MatrixSession,
    config::SyncSettings,
};
use std::sync::Arc;
use tokio::sync::broadcast;

pub mod config;

#[tokio::main]
async fn main() -> Result<()> {
    // Setup logging from .env and RUST_LOG
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("Starting Cumments v2...");

    // 1. Read configuration
    let settings = config::get_configuration().expect("Failed to read configuration.");
    tracing::info!("Configuration loaded successfully.");
    tracing::debug!("Loaded settings: {:?}", settings);

    // 2. Initialize database pool and Store
    let db_pool = sqlx::SqlitePool::connect(&settings.database.url)
        .await
        .expect("Failed to connect to database.");
    tracing::info!("Database pool initialized.");

    let sqlite_store = Arc::new(cumments_store::SqliteStore::new(db_pool.clone()));

    // 3. Initialize Domain Services (Brain)
    let site_service = Arc::new(SiteService::new(sqlite_store.clone()));

    // 4. Initialize Event Bus for real-time updates (SSE)
    let (event_bus, _) = broadcast::channel(100);

    // 5. Initialize Matrix Client if in bot mode
    let matrix_client = if settings.matrix.mode == "bot" {
        tracing::info!("Initializing Matrix client for 'bot' mode.");
        let client = Client::builder()
            .homeserver_url(&settings.matrix.homeserver_url)
            .build()
            .await?;

        let user_id = settings
            .matrix
            .user
            .as_deref()
            .expect("Matrix user not set for bot mode");
        let token = settings
            .matrix
            .token
            .as_deref()
            .expect("Matrix token not set for bot mode");
        let device_id = settings
            .matrix
            .device_id
            .as_deref()
            .expect("Matrix device_id not set for bot mode");

        let session = MatrixSession {
            meta: SessionMeta {
                user_id: user_id.try_into()?,
                device_id: device_id.try_into()?,
            },
            tokens: SessionTokens {
                access_token: token.to_string(),
                refresh_token: None,
            },
        };

        client.restore_session(session).await?;
        tracing::info!("Matrix session restored for user {}.", user_id);
        Some(client)
    } else {
        None
    };

    // 6. Initialize Matrix Driver (Hands) based on configuration
    let driver: Arc<dyn cumments_core::ports::MatrixDriver> = match settings.matrix.mode.as_str() {
        "bot" => {
            let client = matrix_client
                .clone()
                .expect("Matrix client should be initialized");
            let owner_id = settings.matrix.owner_id.clone().try_into()?;
            Arc::new(cumments_matrix::bot::BotMatrixDriver::new(client, owner_id))
        }
        _ => {
            tracing::info!("Using 'logging' mode driver.");
            Arc::new(cumments_matrix::logging::LoggingMatrixDriver)
        }
    };

    // 7. Initialize and run Reconciler (Orchestrator) in the background
    let reconciler = cumments_reconciler::Reconciler::new(
        db_pool.clone(),
        sqlite_store.clone(),
        driver.clone(),
        site_service.clone(),
    );
    tokio::spawn(async move {
        reconciler.run().await;
    });
    tracing::info!("Reconciler started in background.");

    // 8. Initialize Projector (Observer) if in bot mode
    if let Some(client) = matrix_client.clone() {
        let projector = cumments_projector::Projector::new(
            client,
            db_pool.clone(),
            sqlite_store.clone(),
            event_bus.clone(),
        );
        projector.register_handlers();
        tracing::info!("Projector handlers registered.");
    }

    // 9. Wire up API crates
    let pow = cumments_api::pow::Pow::new(
        settings.security.pow_secret,
        settings.security.pow_difficulty,
    );
    let api_state = cumments_api::ApiState {
        store: sqlite_store.clone(),
        pow: Arc::new(pow),
        event_bus: event_bus.clone(),
    };
    tracing::info!("Store and API wired up.");

    // 10. Start Matrix Sync Loop in background if in bot mode
    if let Some(client) = matrix_client {
        tokio::spawn(async move {
            tracing::info!("Starting Matrix sync loop...");
            if let Err(e) = client.sync(SyncSettings::default()).await {
                tracing::error!("Matrix sync loop failed: {:?}", e);
            }
        });
    }

    // 11. Launch the web server
    let address = format!("{}:{}", settings.server.host, settings.server.port);
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .unwrap_or_else(|_| panic!("Failed to bind to address {}", &address));
    tracing::info!("Server listening on {}", listener.local_addr().unwrap());
    axum::serve(
        listener,
        cumments_api::build_router(api_state).into_make_service(),
    )
    .await
    .unwrap();

    Ok(())
}
