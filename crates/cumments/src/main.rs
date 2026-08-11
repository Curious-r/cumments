use anyhow::Result;
use clap::Parser;
use cumments_core::ports::MatrixDriver;
use cumments_core::site_service::SiteService;
use std::io::IsTerminal;
use std::sync::Arc;
use tokio::sync::broadcast;

pub mod cli;
pub mod config;

use config::Mode;

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

    // Logs go to stderr so machine-readable CLI output (e.g. the
    // registration YAML on stdout) stays clean when redirected.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    tracing::info!("Starting Cumments v{}...", env!("CARGO_PKG_VERSION"));

    // ─────────────────────────────────────────────────────────────
    // Handle CLI subcommands
    // ─────────────────────────────────────────────────────────────
    if let Some(cmd) = &args.command {
        match cmd {
            cli::Commands::GenerateRegistration(reg_args) => {
                cli::handle_generate_registration(reg_args, args.config.as_deref())?;
                return Ok(());
            }
            cli::Commands::Backfill(_) => {
                // Handled later, after the driver and processor are wired up.
            }
            cli::Commands::Backup(_) => {
                // Handled after the database is connected.
            }
        }
    }

    // ─────────────────────────────────────────────────────────────
    // 1. Read configuration
    // ─────────────────────────────────────────────────────────────
    match config::resolve_config_path(args.config.as_deref()) {
        Some(path) => tracing::info!("Using config file: {}", path.display()),
        None => tracing::info!("No config file found; using defaults and environment variables."),
    }
    let settings =
        config::get_configuration(args.config.as_deref()).expect("Failed to read configuration.");
    config::validate_pow_secret(&settings.security.pow_secret, settings.matrix.mode)?;
    if settings.matrix.mode == Mode::Logging
        && config::is_known_pow_placeholder(&settings.security.pow_secret)
    {
        tracing::warn!(
            "`security.pow_secret` is the example value `{}`; \
             set a real random secret before switching to appservice mode",
            settings.security.pow_secret
        );
    }
    tracing::info!("Configuration loaded successfully.");

    // ─────────────────────────────────────────────────────────────
    // 2. Initialize database Store
    // ─────────────────────────────────────────────────────────────
    let db_store = Arc::new(
        cumments_store::DbStore::connect(&settings.database.url)
            .await
            .expect("Failed to connect to database."),
    );
    tracing::info!("Database initialized.");

    // Handle backup before any Matrix/driver setup: it only needs SQLite.
    if let Some(cli::Commands::Backup(args)) = &args.command {
        db_store.backup_to(&args.output).await?;
        tracing::info!("Backup written to {}", args.output.display());
        return Ok(());
    }

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
        settings
            .matrix
            .homeserver
            .as_ref()
            .and_then(|h| h.domain.clone()),
    ));
    tracing::info!("EventProcessor initialized.");

    // ─────────────────────────────────────────────────────────────
    // 6. Validate mode and extract validated AppService settings
    // ─────────────────────────────────────────────────────────────
    if settings.matrix.mode == Mode::AppService
        && let Some(appservice) = &settings.matrix.appservice
    {
        match &appservice.registration_file {
            Some(path) => {
                tracing::info!(
                    "Will validate configuration against registration file: {}",
                    path
                )
            }
            None => {
                tracing::warn!(
                    "`matrix.appservice.registration_file` is not set; \
                     registration consistency check is skipped"
                );
            }
        }
    }

    let appservice = match settings.matrix.mode {
        Mode::AppService => Some(settings.matrix.appservice_runtime()?),
        Mode::Logging => None,
    };
    tracing::info!("Matrix mode: {:?}", settings.matrix.mode);

    // ─────────────────────────────────────────────────────────────
    // 7. Initialize Matrix Driver (Hands) based on mode
    // ─────────────────────────────────────────────────────────────
    let driver: Arc<dyn MatrixDriver> = if let Some(as_conf) = &appservice {
        let virtual_user_store: Arc<dyn cumments_core::ports::VirtualUserStore> = db_store.clone();
        tracing::info!(
            "Initializing AppService Matrix driver for {} (domain: {})",
            as_conf.homeserver_url,
            as_conf.server_name
        );
        Arc::new(cumments_matrix::AppServiceMatrixDriver::new(
            as_conf.homeserver_url.clone(),
            as_conf.as_token.clone(),
            as_conf.server_name.clone(),
            as_conf.sender_localpart.clone(),
            as_conf.owner_id.clone(),
            virtual_user_store,
            as_conf.room_version.clone(),
        ))
    } else {
        tracing::info!("Using 'logging' mode driver.");
        Arc::new(cumments_matrix::logging::LoggingMatrixDriver)
    };

    // ─────────────────────────────────────────────────────────────
    // 7b. Handle the backfill subcommand (needs driver + processor)
    // ─────────────────────────────────────────────────────────────
    if let Some(cmd) = &args.command {
        match cmd {
            cli::Commands::GenerateRegistration(_) => unreachable!("handled earlier"),
            cli::Commands::Backfill(args) => {
                let backfiller = cumments_projector::backfill::Backfiller::new(
                    driver.clone(),
                    event_processor.clone(),
                    db_store.clone(),
                    db_store.clone(),
                    db_store.clone(),
                );
                let summary = backfiller.run(args.max_pages).await?;
                tracing::info!(
                    "Backfill complete: {} rooms, {} events",
                    summary.rooms,
                    summary.events
                );
                return Ok(());
            }
            cli::Commands::Backup(_) => unreachable!("handled earlier"),
        }
    }

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
    // 9b. Ensure the human owner keeps admin power in every known
    //     Cumments room (appservice mode, best-effort)
    // ─────────────────────────────────────────────────────────────
    if appservice.is_some() {
        let sweep_driver = driver.clone();
        tokio::spawn(async move {
            // The homeserver may not be ready when this task starts (compose
            // `depends_on` only waits for container start, not readiness).
            // Try immediately and retry at a fixed interval until it answers
            // or the retry budget runs out; no artificial startup delay is
            // needed because the interval only applies between attempts.
            const SWEEP_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
            const SWEEP_MAX_RETRIES: usize = 10;

            let mut retries = 0;
            let rooms = loop {
                match sweep_driver.get_joined_rooms().await {
                    Ok(rooms) => break rooms,
                    Err(e) => {
                        retries += 1;
                        if retries > SWEEP_MAX_RETRIES {
                            tracing::warn!(
                                "Owner admin sweep: giving up after {} retries: {:?}",
                                retries - 1,
                                e
                            );
                            return;
                        }
                        tracing::warn!(
                            "Owner admin sweep: failed to list joined rooms (retry {}/{}): {:?}",
                            retries,
                            SWEEP_MAX_RETRIES,
                            e
                        );
                        tokio::time::sleep(SWEEP_RETRY_INTERVAL).await;
                    }
                }
            };
            for room_id in rooms {
                match sweep_driver.get_room_metadata(&room_id).await {
                    Ok(Some(meta)) if meta.get("site_id").and_then(|v| v.as_str()).is_some() => {
                        sweep_driver.ensure_owner_admin(&room_id).await;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(
                            "Owner admin sweep: failed to read metadata for {}: {:?}",
                            room_id,
                            e
                        );
                    }
                }
            }
        });
        tracing::info!("Owner admin sweep started in background.");
    }

    // ─────────────────────────────────────────────────────────────
    // 10. Start Event Receiver based on mode
    // ─────────────────────────────────────────────────────────────
    match &appservice {
        Some(as_conf) => {
            // PushReceiver – listens for HS push events
            let push_port = as_conf.listen_port;

            if push_port == settings.server.port {
                tracing::info!(
                    "PushReceiver will share port {} with the API server.",
                    push_port
                );
                // Routes will be merged in step 12
            } else {
                tracing::info!("Starting PushReceiver on separate port {}.", push_port);
                let host = as_conf.listen_host.clone();
                let listener = tokio::net::TcpListener::bind((host.as_str(), push_port))
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to bind PushReceiver to {}:{}: {}",
                            host,
                            push_port,
                            e
                        )
                    })?;
                tracing::info!("PushReceiver listening on {}", listener.local_addr()?);
                let push_app = cumments_projector::push_receiver::push_router_standalone(
                    event_processor.clone(),
                    as_conf.hs_token.clone(),
                );
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener, push_app.into_make_service()).await {
                        tracing::error!("PushReceiver server error: {:#}", e);
                    }
                });
            }
        }
        None => {
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
    let api_router = cumments_api::build_router(api_state, &settings.server.cors_origins);

    // ─────────────────────────────────────────────────────────────
    // 12. Assemble final router (merge push routes if shared port)
    // ─────────────────────────────────────────────────────────────
    let final_router = if let Some(as_conf) = &appservice {
        if as_conf.listen_port == settings.server.port {
            // Merge push routes into the API server
            let push_router = cumments_projector::push_receiver::push_router(
                event_processor,
                as_conf.hs_token.clone(),
            );
            api_router.merge(push_router)
        } else {
            api_router
        }
    } else {
        api_router
    };

    // ─────────────────────────────────────────────────────────────
    // 13. Launch the web server
    // ─────────────────────────────────────────────────────────────
    let listener =
        tokio::net::TcpListener::bind((settings.server.host.as_str(), settings.server.port))
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to bind to {}:{}: {}",
                    settings.server.host,
                    settings.server.port,
                    e
                )
            })?;
    tracing::info!("Server listening on {}", listener.local_addr()?);
    axum::serve(listener, final_router.into_make_service())
        .await
        .map_err(|e| anyhow::anyhow!("HTTP server error: {:#}", e))?;

    Ok(())
}
