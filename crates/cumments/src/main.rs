use anyhow::Result;
use clap::{CommandFactory, Parser};
use cumments_core::ports::{CommandAuditStore, MatrixDriver};
use cumments_core::site_service::SiteService;
use std::io::IsTerminal;
use std::sync::Arc;
use tokio::sync::broadcast;

pub mod cli;
pub mod config;

use cli::CliError;
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
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::from(u8::try_from(error.kind().exit_code()).unwrap_or(1))
        }
    }
}

async fn run() -> Result<(), CliError> {
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
            cli::Commands::Database(_) => {
                // Handled after the database is connected.
            }
            cli::Commands::Sites(_) => {
                // Handled after the database is connected.
            }
            cli::Commands::Pages(_) => {
                // Handled after the database and Matrix adapter are ready.
            }
            cli::Commands::Rooms(_) => {
                // Handled after the database is connected.
            }
            cli::Commands::QuarantinedRooms(_) => {
                // Handled after the database is connected.
            }
            cli::Commands::Audit(_) => {
                // Handled after the database is connected.
            }
            cli::Commands::ProjectionRepairs(_) => {
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
    let settings = config::get_configuration(args.config.as_deref()).map_err(|error| {
        cli::CliError::validation(format!("failed to read configuration: {error}"))
    })?;
    config::validate_pow_secret(&settings.security.pow_secret, settings.matrix.mode)
        .map_err(|error| cli::CliError::validation(error.to_string()))?;
    let operator_token_hash = config::operator_token_hash(&settings.security)
        .map_err(|error| cli::CliError::validation(error.to_string()))?;
    let operator_mxids = config::validate_operator_mxids(&settings.security)
        .map_err(|error| cli::CliError::validation(error.to_string()))?;
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
    let site_auth_policy = Arc::new(
        config::build_site_auth_policy(&settings.security, &settings.sites)
            .map_err(|error| cli::CliError::validation(error.to_string()))?,
    );

    // ─────────────────────────────────────────────────────────────
    // 2. Initialize database Store
    // ─────────────────────────────────────────────────────────────
    let db_store = Arc::new(
        cumments_store::DbStore::connect(&settings.database.url)
            .await
            .map_err(|error| {
                cli::CliError::dependency("failed to connect to database", anyhow::anyhow!(error))
            })?,
    );
    tracing::info!("Database initialized.");

    // Handle CLI subcommands that only need the database.
    if let Some(cli::Commands::QuarantinedRooms(args)) = &args.command {
        cli::handle_quarantined_rooms_command(&db_store, args).await?;
        return Ok(());
    }
    if let Some(cli::Commands::Audit(audit_args)) = &args.command {
        let cli::AuditCommand::Entries(entries_args) = &audit_args.command;
        let cli::AuditEntriesCommand::List(list_args) = &entries_args.command;
        let entries = db_store
            .list_command_audit(list_args.actor.as_deref(), list_args.limit)
            .await?;
        let total = db_store
            .count_command_audit(list_args.actor.as_deref())
            .await? as i64;
        cli::print_list(&entries, total, 1, list_args.limit as i64)?;
        return Ok(());
    }
    if let Some(cli::Commands::ProjectionRepairs(args)) = &args.command {
        cli::handle_projection_repairs_command(&db_store, args).await?;
        return Ok(());
    }

    // Handle backup before any Matrix/driver setup: it only needs SQLite.
    if let Some(cli::Commands::Database(database_args)) = &args.command
        && let cli::DatabaseCommand::Backups(backups_args) = &database_args.command
        && let cli::DatabaseBackupsCommand::Create(args) = &backups_args.command
    {
        db_store.backup_to(&args.output).await?;
        tracing::info!("Backup written to {}", args.output.display());
        return Ok(());
    }

    // ─────────────────────────────────────────────────────────────
    // 3. Initialize Domain Services (Brain)
    // ─────────────────────────────────────────────────────────────
    let site_service = Arc::new(SiteService::new(db_store.clone()));

    // DB-only site commands run here; only applied-role removal needs the
    // Matrix driver and is deferred until after driver setup.
    let sites_deferred = if let Some(cli::Commands::Sites(sites_args)) = &args.command {
        let needs_driver = matches!(
            &sites_args.command,
            cli::SitesCommand::Admins(cli::SiteAdminsArgs {
                command: cli::SiteAdminsCommand::Remove(_),
            }) | cli::SitesCommand::Managers(cli::SiteManagersArgs {
                command: cli::SiteManagersCommand::Remove(_),
            }) | cli::SitesCommand::Moderators(cli::PageModeratorsArgs {
                command: cli::PageModeratorsCommand::Remove(_),
            }) | cli::SitesCommand::Packs(cli::SitePacksArgs {
                command: cli::SitePacksCommand::Stickers(_)
            })
        );
        if !needs_driver {
            let logging = cumments_matrix::LoggingMatrixDriver;
            cli::handle_sites_command(
                &db_store,
                &logging,
                &site_service,
                &site_auth_policy,
                sites_args,
            )
            .await?;
            return Ok(());
        }
        true
    } else {
        false
    };

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
    if sites_deferred && let Some(cli::Commands::Sites(sites_args)) = &args.command {
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

    if let Some(cli::Commands::Pages(pages_args)) = &args.command {
        cli::handle_pages_command(&db_store, driver.as_ref(), &site_service, pages_args).await?;
        return Ok(());
    }

    // Room upgrades need the driver + site service; they run here instead
    // of the early database-only phase.
    if let Some(cli::Commands::Rooms(rooms_args)) = &args.command
        && let cli::RoomsCommand::Upgrades(upgrades_args) = &rooms_args.command
        && let cli::RoomUpgradesCommand::Create(upgrade_args) = &upgrades_args.command
    {
        cli::handle_rooms_upgrade_command(&db_store, driver.as_ref(), &site_service, upgrade_args)
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
            sticker_pack_store: db_store.clone(),
            projection_repair_store: db_store.clone(),
            role_claim_store: db_store.clone(),
            submission_store: db_store.clone(),
            audit_store: db_store.clone(),
            site_auth_store: db_store.clone(),
            site_auth_policy: site_auth_policy.clone(),
            site_service: site_service.clone(),
            driver: Some(driver.clone()),
            operator_mxids: operator_mxids.clone(),
            backfill_tx: Some(backfill_tx),
            event_bus: event_bus.clone(),
            governance_notify: governance_notify.clone(),
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

    // Publish projector events only after their facts are committed. Frequent
    // polling keeps live latency low without coupling publication to the HTTP
    // response that acknowledged the homeserver transaction.
    {
        let outbox_store = db_store.clone();
        let event_bus = event_bus.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
            loop {
                interval.tick().await;
                if let Err(error) = cumments_projector::sse_outbox::publish_pending(
                    outbox_store.as_ref(),
                    &event_bus,
                )
                .await
                {
                    tracing::error!("SSE outbox publisher failed: {error:#}");
                }
            }
        });
    }

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
            cli::Commands::Database(_) => unreachable!("handled earlier"),
            cli::Commands::Sites(_) => unreachable!("handled earlier"),
            cli::Commands::Pages(_) => unreachable!("handled earlier"),
            cli::Commands::QuarantinedRooms(_) => unreachable!("handled earlier"),
            cli::Commands::Audit(_) => unreachable!("handled earlier"),
            cli::Commands::Rooms(_) => unreachable!("handled earlier"),
            cli::Commands::ProjectionRepairs(_) => unreachable!("handled earlier"),
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
            projection_repair_store: db_store.clone(),
            message_store: db_store.clone(),
            room_store: db_store.clone(),
            virtual_user_store: db_store.clone(),
            site_auth_store: db_store.clone(),
            site_transfer_store: db_store.clone(),
            state_redaction_repairer: event_processor.clone(),
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
                    db_store.clone() as Arc<dyn cumments_core::ports::AppServiceTxnStore>,
                    db_store.clone() as Arc<dyn cumments_core::ports::SseOutboxStore>,
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
                runtime.as_token.clone(),
                media_sign_key,
                settings.server.public_base_url.clone(),
                settings.security.media_proxy_allow_private_servers,
            )
            .expect("build media proxy"),
        )
    });
    let rate_limits = settings.rate_limit.resolved()?;
    let api_state = cumments_api::ApiState {
        store: db_store.clone(),
        driver: driver.clone(),
        site_service: site_service.clone(),
        pow: Arc::new(pow),
        event_bus,
        submission_notify,
        governance_notify,
        site_auth_policy,
        operator_token_hash,
        server_name: settings
            .matrix
            .homeserver
            .as_ref()
            .and_then(|h| h.domain.clone())
            .or_else(|| {
                // Fallback: derive from sender MXID when domain is implicit.
                appservice.as_ref().map(|c| c.server_name.clone())
            }),
        registration_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.registration.requests,
            rate_limits.registration.window,
        )),
        verification_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.verification.requests,
            rate_limits.verification.window,
        )),
        operator_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.operator.requests,
            rate_limits.operator.window,
        )),
        claim_token_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.claim_token.requests,
            rate_limits.claim_token.window,
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
        sse_limiter: Arc::new(cumments_api::rate_limit::SseRateLimiter::new(
            rate_limits.sse.requests,
            rate_limits.sse.window,
            rate_limits.sse.burst,
        )),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(500)),
        media_proxy,
        media_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.media.requests,
            rate_limits.media.window,
        )),
        visitor_profile_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.visitor_profile.requests,
            rate_limits.visitor_profile.window,
        )),
        public_read_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.public_read.requests,
            rate_limits.public_read.window,
        )),
        governance_limiter: Arc::new(cumments_api::rate_limit::RateLimiter::new(
            rate_limits.governance.requests,
            rate_limits.governance.window,
        )),
        ephemeral_bus: ephemeral_bus.clone(),
        ephemeral_state: Some(ephemeral_state),
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
                db_store.clone() as Arc<dyn cumments_core::ports::AppServiceTxnStore>,
                db_store.clone() as Arc<dyn cumments_core::ports::SseOutboxStore>,
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
