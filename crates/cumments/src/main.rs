use anyhow::Result;

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

    // 2. Initialize database pool
    let db_pool = sqlx::SqlitePool::connect(&settings.database.url)
        .await
        .expect("Failed to connect to database.");
    tracing::info!("Database pool initialized.");

    // 3. Initialize operator based on configuration
    let operator: std::sync::Arc<dyn cumments_operator::MatrixOperator> =
        match settings.matrix.mode.as_str() {
            "bot" => {
                tracing::info!("Using 'bot' mode operator.");
                let bot_operator = cumments_operator::bot::BotOperator::new(
                    &settings.matrix.homeserver_url,
                    settings
                        .matrix
                        .user
                        .as_deref()
                        .expect("Matrix user not set for bot mode"),
                    settings
                        .matrix
                        .token
                        .as_deref()
                        .expect("Matrix token not set for bot mode"),
                    settings.matrix.device_id.as_deref(),
                )
                .await
                .expect("Failed to create BotOperator");
                std::sync::Arc::new(bot_operator)
            }
            _ => {
                tracing::info!("Using 'logging' mode operator.");
                std::sync::Arc::new(cumments_operator::logging::LoggingOperator)
            }
        };

    // 4. Initialize and run reconciler in the background
    let reconciler = cumments_reconciler::Reconciler::new(db_pool.clone(), operator.clone());
    tokio::spawn(async move {
        reconciler.run().await;
    });
    tracing::info!("Reconciler started in background.");

    // 5. Initialize and run projectionist in the background
    if settings.matrix.mode.as_str() == "bot" {
        let projection_pool = db_pool.clone();
        let projectionist = cumments_projection::Projection::new(
            projection_pool,
            &settings.matrix.homeserver_url,
            settings
                .matrix
                .user
                .as_deref()
                .expect("Matrix user not set for bot mode"),
            settings
                .matrix
                .token
                .as_deref()
                .expect("Matrix token not set for bot mode"),
            settings.matrix.device_id.as_deref(),
        )
        .await
        .expect("Failed to create Projectionist");

        tokio::spawn(async move {
            if let Err(e) = projectionist.run().await {
                tracing::error!("Projectionist failed: {:?}", e);
            }
        });
        tracing::info!("Projectionist started in background.");
    }

    // 6. Wire up storage and API crates
    let storage = cumments_storage::Storage::new(db_pool);
    let api_state = cumments_api::ApiState {
        storage: std::sync::Arc::new(storage),
    };
    tracing::info!("Storage and API wired up.");

    // 6. Launch the web server
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
