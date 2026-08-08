use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Settings {
    pub server: Server,
    pub database: Database,
    pub security: Security,
    pub matrix: Matrix,
}

#[derive(Debug, Deserialize)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub cors_origins: String,
}

#[derive(Debug, Deserialize)]
pub struct Database {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct Security {
    pub pow_secret: String,
    pub pow_difficulty: u32,
}

/// Configuration for Matrix connectivity.
/// Fields will be present or not depending on the `mode`.
#[derive(Debug, Deserialize)]
pub struct Matrix {
    /// Operation mode: "appservice" | "logging"
    pub mode: String,
    pub homeserver_url: String,
    pub owner_id: String,

    // ── AppService mode fields ──
    /// AppService token for authenticating with the homeserver
    pub as_token: Option<String>,
    /// Homeserver token for verifying incoming push requests
    pub hs_token: Option<String>,
    /// The server name (domain) part of Matrix IDs
    pub server_name: Option<String>,
    /// Localpart for the AppService's sender user (default: "cumments")
    pub sender_localpart: Option<String>,
    /// Port for the push receiver endpoint (default: 3001)
    /// Set to the same value as server.port to share the main listener
    pub push_listen_port: Option<u16>,
}

/// Reads configuration from a file and environment variables.
/// If `config_path` is provided, it loads that specific file.
/// Otherwise, it looks for `config.toml` (or other supported formats) in the current directory.
pub fn get_configuration(config_path: Option<&str>) -> Result<Settings, config::ConfigError> {
    let mut builder = config::Config::builder();

    if let Some(path) = config_path {
        // Load specific file
        builder = builder.add_source(config::File::with_name(path).required(true));
    } else {
        // Fallback to default search
        builder = builder.add_source(config::File::with_name("config").required(false));
    }

    let settings = builder
        .set_default("server.port", 7931)?
        .set_default("server.host", "localhost")?
        .set_default("matrix.push_listen_port", 3001)?
        .set_default("matrix.sender_localpart", "cumments")?
        // Add in environment variables with a prefix of CUMMENTS and separator __
        // e.g. `CUMMENTS_SERVER__PORT=5000` would override `port` in `[server]`
        .add_source(config::Environment::with_prefix("CUMMENTS").separator("__"))
        .build()?;

    settings.try_deserialize()
}
