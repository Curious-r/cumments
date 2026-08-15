use anyhow::Result;
use clap::{CommandFactory, Parser};
use cumments_core::ports::{CommandAuditStore, MatrixDriver};
use cumments_core::site_service::SiteService;
use std::io::IsTerminal;
use std::sync::Arc;
use tokio::sync::broadcast;

pub mod cli;
pub mod config;

use config::Mode;

/// The complete clap command, used by `cumments completions`.
pub(crate) fn cli_command() -> clap::Command {
    Args::command()
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the configuration file (accepted before or after a subcommand)
    #[arg(short, long, global = true)]
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
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,sqlx=warn"));

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
            cli::Commands::Appservice(appservice_args) => {
                cli::handle_appservice_command(appservice_args, args.config.as_deref())?;
                return Ok(());
            }
            cli::Commands::Backfill(_) => {
                // Handled later, after the driver and processor are wired up.
            }
            cli::Commands::Backup(_) => {
                // Handled after the database is connected.
            }
            cli::Commands::Sites(_) => {
                // Handled after the database is connected.
            }
            cli::Commands::Rooms(_) => {
                // Handled after the database is connected.
            }
            cli::Commands::Audit(_) => {
                // Handled after the database is connected.
            }
            cli::Commands::Completions(args) => {
                cli::handle_completions(args)?;
                return Ok(());
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
    let settings = config::get_configuration(args.config.as_deref())
        .map_err(|e| anyhow::anyhow!("failed to read configuration: {e}"))?;
    config::validate_pow_secret(&settings.security.pow_secret, settings.matrix.mode)?;
    let admin_token_hash = config::admin_token_hash(&settings.security)?;
    let admin_mxids = config::validate_admin_mxids(&settings.security)?;
    let (backfill_tx, backfill_rx) = tokio::sync::mpsc::channel(1);
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
    let site_auth_policy = Arc::new(config::build_site_auth_policy(
        &settings.security,
        &settings.sites,
    )?);

    // ─────────────────────────────────────────────────────────────
    // 2. Initialize database Store
    // ─────────────────────────────────────────────────────────────
    let db_store = Arc::new(
        cumments_store::DbStore::connect(&settings.database.url)
            .await
            .map_err(|e| anyhow::anyhow!("failed to connect to database: {e}"))?,
    );
    tracing::info!("Database initialized.");

    // Handle CLI subcommands that only need the database.
    if let Some(cli::Commands::Rooms(rooms_args)) = &args.command {
        cli::handle_rooms_command(&db_store, rooms_args).await?;
        return Ok(());
    }
    if let Some(cli::Commands::Audit(audit_args)) = &args.command {
        let entries = db_store
            .list_command_audit(audit_args.actor.as_deref(), audit_args.limit)
            .await?;
        cli::print_json(&entries)?;
        return Ok(());
    }

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
    let submission_notify = Arc::new(tokio::sync::Notify::new());
    let governance_notify = Arc::new(tokio::sync::Notify::new());
    let projection_notify = Arc::new(tokio::sync::Notify::new());

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

    // Ephemeral event channel (typing/receipts/presence) for SSE.
    let (ephemeral_bus, _) = tokio::sync::broadcast::channel(256);
    let ephemeral_state = cumments_core::ephemeral::EphemeralState::new();
    if let Some(runtime) = &appservice {
        let ephemeral_sync = cumments_projector::ephemeral::EphemeralSync::new(
            runtime.homeserver_url.clone(),
            runtime.as_token.clone(),
            format!("@{}:{}", runtime.sender_localpart, runtime.server_name),
            db_store.clone(),
            db_store.clone(),
            ephemeral_state.clone(),
            ephemeral_bus.clone(),
        )
        .expect("build ephemeral sync");
        tokio::spawn(async move { ephemeral_sync.run().await });
    }

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
            virtual_user_store,
            as_conf.room_version.clone(),
        )?)
    } else {
        tracing::info!("Using 'logging' mode driver.");
        Arc::new(cumments_matrix::logging::LoggingMatrixDriver)
    };

    // CLI site commands need the driver for applied-role removal; they run
    // after driver setup instead of the early database-only phase.
    if let Some(cli::Commands::Sites(sites_args)) = &args.command {
        cli::handle_sites_command(
            &db_store,
            driver.as_ref(),
            &site_service,
            &site_auth_policy,
            sites_args,
        )
        .await?;
        return Ok(());
    }

    // ─────────────────────────────────────────────────────────────
    // 7a. Initialize EventProcessor (shared across all modes)
    // ─────────────────────────────────────────────────────────────
    let event_processor = Arc::new(cumments_projector::event_processor::EventProcessor::new(
        cumments_projector::event_processor::EventProcessorDeps {
            site_store: db_store.clone(),
            registry_store: db_store.clone(),
            message_store: db_store.clone(),
            room_store: db_store.clone(),
            governance_store: db_store.clone(),
            role_claim_store: db_store.clone(),
            submission_store: db_store.clone(),
            audit_store: db_store.clone(),
            site_auth_store: db_store.clone(),
            site_service: site_service.clone(),
            driver: Some(driver.clone()),
            admin_mxids: admin_mxids.clone(),
            backfill_tx: Some(backfill_tx),
            event_bus: event_bus.clone(),
            projection_notify: projection_notify.clone(),
            server_name: settings
                .matrix
                .homeserver
                .as_ref()
                .and_then(|h| h.domain.clone()),
        },
    ));
    tracing::info!("EventProcessor initialized.");

    // Bot-triggered backfill runs in a single worker: one job at a time,
    // completion reported back as a DM to the requester.
    let backfill_worker = cumments_projector::backfill::BackfillWorker::new(
        backfill_rx,
        driver.clone(),
        event_processor.clone(),
        db_store.clone(),
        db_store.clone(),
        db_store.clone(),
    );
    tokio::spawn(backfill_worker.run());

    // ─────────────────────────────────────────────────────────────
    // 7b. Handle the backfill subcommand (needs driver + processor)
    // ─────────────────────────────────────────────────────────────
    if let Some(cmd) = &args.command {
        match cmd {
            cli::Commands::Appservice(_) => unreachable!("handled earlier"),
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
            cli::Commands::Sites(_) => unreachable!("handled earlier"),
            cli::Commands::Audit(_) => unreachable!("handled earlier"),
            cli::Commands::Rooms(_) => unreachable!("handled earlier"),
            cli::Commands::Completions(_) => unreachable!("handled earlier"),
        }
    }

    // ─────────────────────────────────────────────────────────────
    // 9. Initialize and run Reconciler (Orchestrator)
    // ─────────────────────────────────────────────────────────────
    let reconciler = cumments_reconciler::Reconciler::new(
        cumments_reconciler::ReconcilerDeps {
            submission_store: db_store.clone(),
            registry_store: db_store.clone(),
            site_store: db_store.clone(),
            role_claim_store: db_store.clone(),
            governance_store: db_store.clone(),
            message_store: db_store.clone(),
            virtual_user_store: db_store.clone(),
            site_auth_store: db_store.clone(),
            driver: driver.clone(),
            site_service: site_service.clone(),
        },
        cumments_reconciler::PassWakeups {
            submission: submission_notify.clone(),
            governance: governance_notify.clone(),
            projection: projection_notify.clone(),
        },
    );
    tokio::spawn(async move {
        reconciler.run().await;
    });
    tracing::info!("Reconciler started in background.");

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
    let trusted_proxies = cumments_api::trusted_proxy::TrustedProxySet::from_rules(
        settings.server.trusted_proxies.as_slice(),
    )?;
    let media_proxy = appservice.as_ref().map(|runtime| {
        let media_sign_key = settings.security.media_sign_key.clone().unwrap_or_else(|| {
            tracing::warn!(
                "security.media_sign_key is not set; media proxy URLs are signed \
                 with the AppService token and will all expire when it rotates"
            );
            runtime.as_token.clone()
        });
        Arc::new(
            cumments_api::routes::media::MediaProxy::new(
                runtime.homeserver_url.clone(),
                runtime.server_name.clone(),
                runtime.as_token.clone(),
                media_sign_key,
            )
            .expect("build media proxy"),
        )
    });
    let rate_limits = settings.rate_limit.resolved()?;
    let api_state = cumments_api::ApiState {
        store: db_store,
        driver: driver.clone(),
        site_service: site_service.clone(),
        pow: Arc::new(pow),
        event_bus,
        submission_notify,
        governance_notify,
        site_auth_policy,
        admin_token_hash,
        registration_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.registration.requests,
            rate_limits.registration.window,
        )),
        verification_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.verification.requests,
            rate_limits.verification.window,
        )),
        admin_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.admin.requests,
            rate_limits.admin.window,
        )),
        confirm_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.confirm.requests,
            rate_limits.confirm.window,
        )),
        trusted_proxies: Arc::new(trusted_proxies),
        allow_private_verification_origins: settings.security.allow_private_verification_origins,
        write_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.write.requests,
            rate_limits.write.window,
        )),
        sse_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.sse.requests,
            rate_limits.sse.window,
        )),
        sse_reconnect: Arc::new(std::sync::Mutex::new(
            cumments_api::routes::sse::SseReconnectRegistry::default(),
        )),
        max_sse_connections: 500,
        active_sse_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        media_proxy,
        media_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.media.requests,
            rate_limits.media.window,
        )),
        moderation_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.moderation.requests,
            rate_limits.moderation.window,
        )),
        ephemeral_bus: ephemeral_bus.clone(),
        ephemeral_state: Some(ephemeral_state),
        preset_stickers: Arc::new(settings.security.preset_stickers.clone()),
    };
    let api_router = cumments_api::build_router(api_state);

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
    axum::serve(
        listener,
        final_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("HTTP server error: {:#}", e))?;

    Ok(())
}
