use anyhow::{Result, anyhow};
use clap::Parser;
use cumments_core::site_service::SiteService;
use matrix_sdk::{
    Client, SessionMeta, authentication::SessionTokens, authentication::matrix::MatrixSession,
    config::SyncSettings,
};
use std::sync::Arc;
use tokio::sync::broadcast;

pub mod cli;
pub mod config;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the configuration file
    #[arg(short, long)]
    config: Option<String>,

    /// Optional subcommand
    #[command(subcommand)]
    command: Option<cli::Commands>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Setup logging from .env and RUST_LOG
    dotenvy::dotenv().ok();

    // Parse CLI arguments
    let args = Args::parse();

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::fmt().with_env_filter(filter).init();

    tracing::info!("Starting Cumments v{}...", env!("CARGO_PKG_VERSION"));

    // ─────────────────────────────────────────────────────────────
    // Handle CLI subcommands
    // ─────────────────────────────────────────────────────────────
    if let Some(cmd) = &args.command {
        match cmd {
            cli::Commands::GenerateRegistration(args) => {
                cli::handle_generate_registration(args)?;
                return Ok(());
            }
        }
    }

    // ─────────────────────────────────────────────────────────────
    // 1. Read configuration
    // ─────────────────────────────────────────────────────────────
    let settings =
        config::get_configuration(args.config.as_deref()).expect("Failed to read configuration.");
    tracing::info!("Configuration loaded successfully.");
    tracing::debug!("Loaded settings: {:?}", settings);

    // ─────────────────────────────────────────────────────────────
    // 2. Initialize database Store
    // ─────────────────────────────────────────────────────────────
    let db_store = Arc::new(
        cumments_store::DbStore::connect(&settings.database.url)
            .await
            .expect("Failed to connect to database."),
    );
    tracing::info!("Database initialized.");

    // ─────────────────────────────────────────────────────────────
    // 3. Initialize Domain Services (Brain)
    // ─────────────────────────────────────────────────────────────
    let site_service = Arc::new(SiteService::new(db_store.clone()));

    // ─────────────────────────────────────────────────────────────
    // 4. Initialize Event Bus for real-time updates (SSE)
    // ─────────────────────────────────────────────────────────────
    let (event_bus, _) = broadcast::channel(100);

    // ─────────────────────────────────────────────────────────────
    // 5. Initialize EventProcessor (shared across all modes)
    // ─────────────────────────────────────────────────────────────
    let event_processor = Arc::new(cumments_projector::event_processor::EventProcessor::new(
        db_store.clone(), // SiteStore
        db_store.clone(), // RegistryStore
        db_store.clone(), // CommentStore
        db_store.clone(), // IntentStore
        event_bus.clone(),
    ));
    tracing::info!("EventProcessor initialized.");

    // ─────────────────────────────────────────────────────────────
    // 6. Initialize Matrix Client (bot mode only)
    // ─────────────────────────────────────────────────────────────
    let mode = settings.matrix.mode.as_str();
    let matrix_client = if mode == "bot" {
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

    // ─────────────────────────────────────────────────────────────
    // 7. Initialize Matrix Driver (Hands) based on mode
    // ─────────────────────────────────────────────────────────────
    let driver: Arc<dyn cumments_core::ports::MatrixDriver> = match mode {
        "bot" => {
            let client = matrix_client
                .clone()
                .expect("Matrix client should be initialized in bot mode");
            let owner_id = settings.matrix.owner_id.clone().try_into()?;
            Arc::new(cumments_matrix::bot::BotMatrixDriver::new(
                client, owner_id,
            )?)
        }
        "appservice" => {
            let as_token = settings
                .matrix
                .as_token
                .as_deref()
                .ok_or_else(|| anyhow!("`as_token` is required for appservice mode"))?;
            let _hs_token = settings
                .matrix
                .hs_token
                .as_deref()
                .ok_or_else(|| anyhow!("`hs_token` is required for appservice mode"))?;
            let server_name = settings
                .matrix
                .server_name
                .as_deref()
                .ok_or_else(|| anyhow!("`server_name` is required for appservice mode"))?;
            let bot_localpart = settings
                .matrix
                .bot_localpart
                .clone()
                .unwrap_or_else(|| "cumments".to_string());
            let owner_id = settings.matrix.owner_id.clone();
            let virtual_user_store: Arc<dyn cumments_core::ports::VirtualUserStore> =
                db_store.clone();

            tracing::info!(
                "Initializing AppService Matrix driver on {}",
                settings.matrix.homeserver_url
            );
            Arc::new(cumments_matrix::AppServiceMatrixDriver::new(
                settings.matrix.homeserver_url.clone(),
                as_token.to_string(),
                server_name.to_string(),
                bot_localpart,
                owner_id,
                virtual_user_store,
            ))
        }
        _ => {
            tracing::info!("Using 'logging' mode driver.");
            Arc::new(cumments_matrix::logging::LoggingMatrixDriver)
        }
    };

    // ─────────────────────────────────────────────────────────────
    // 8. Initialize Shared Coordination Signals
    // ─────────────────────────────────────────────────────────────
    let reconciler_notify = Arc::new(tokio::sync::Notify::new());

    // ─────────────────────────────────────────────────────────────
    // 9. Initialize and run Reconciler (Orchestrator)
    // ─────────────────────────────────────────────────────────────
    let reconciler = cumments_reconciler::Reconciler::new(
        db_store.clone(), // IntentStore
        db_store.clone(), // RegistryStore
        db_store.clone(), // CommentStore
        driver.clone(),
        site_service.clone(),
        reconciler_notify.clone(),
    );
    tokio::spawn(async move {
        reconciler.run().await;
    });
    tracing::info!("Reconciler started in background.");

    // ─────────────────────────────────────────────────────────────
    // 10. Start Event Receiver based on mode
    // ─────────────────────────────────────────────────────────────
    match mode {
        "bot" => {
            // SyncAdapter (Projector) – uses matrix-sdk event handlers
            if let Some(ref client) = matrix_client {
                let projector =
                    cumments_projector::Projector::new(client.clone(), event_processor.clone());
                projector.register_handlers();
                tracing::info!("SyncAdapter (Projector) handlers registered.");
            }
        }
        "appservice" => {
            // PushReceiver – listens for HS push events
            let push_port = settings.matrix.push_listen_port.unwrap_or(3001);

            let push_app = cumments_projector::push_receiver::push_router(event_processor.clone());

            if push_port == settings.server.port {
                tracing::info!(
                    "PushReceiver will share port {} with the API server.",
                    push_port
                );
                // Routes will be merged in step 12
            } else {
                tracing::info!("Starting PushReceiver on separate port {}.", push_port);
                let host = settings.server.host.clone();
                tokio::spawn(async move {
                    let listener = tokio::net::TcpListener::bind((host.as_str(), push_port))
                        .await
                        .unwrap_or_else(|e| {
                            panic!("Failed to bind PushReceiver to port {}: {}", push_port, e)
                        });
                    tracing::info!(
                        "PushReceiver listening on {}",
                        listener.local_addr().unwrap()
                    );
                    axum::serve(listener, push_app.into_make_service())
                        .await
                        .unwrap();
                });
            }
        }
        _ => {
            // Logging mode – no event receiver needed
        }
    }

    // ─────────────────────────────────────────────────────────────
    // 11. Wire up API crate
    // ─────────────────────────────────────────────────────────────
    let pow = cumments_api::pow::Pow::new(
        settings.security.pow_secret,
        settings.security.pow_difficulty,
    );
    let api_state = cumments_api::ApiState {
        store: db_store,
        pow: Arc::new(pow),
        event_bus,
        reconciler_notify,
    };
    let api_router = cumments_api::build_router(api_state);

    // ─────────────────────────────────────────────────────────────
    // 12. Assemble final router (merge push routes if shared port)
    // ─────────────────────────────────────────────────────────────
    let final_router = if mode == "appservice"
        && settings.matrix.push_listen_port.unwrap_or(3001) == settings.server.port
    {
        // Merge push routes into the API server
        let push_router = cumments_projector::push_receiver::push_router(event_processor);
        api_router.merge(push_router)
    } else {
        api_router
    };

    // ─────────────────────────────────────────────────────────────
    // 13. Start Matrix Sync Loop (bot mode only)
    // ─────────────────────────────────────────────────────────────
    if let Some(client) = matrix_client {
        tokio::spawn(async move {
            tracing::info!("Starting Matrix sync loop...");
            if let Err(e) = client.sync(SyncSettings::default()).await {
                tracing::error!("Matrix sync loop failed: {:?}", e);
            }
        });
    }

    // ─────────────────────────────────────────────────────────────
    // 14. Launch the web server
    // ─────────────────────────────────────────────────────────────
    let listener =
        tokio::net::TcpListener::bind((settings.server.host.as_str(), settings.server.port))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "Failed to bind to {}:{}",
                    settings.server.host, settings.server.port
                )
            });
    tracing::info!("Server listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, final_router.into_make_service())
        .await
        .unwrap();

    Ok(())
}
