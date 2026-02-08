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
    pub public_server_name: String,
    pub cors_origins: String,
}

#[derive(Debug, Deserialize)]
pub struct Database {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct Security {
    pub identity_salt: String,
    pub admin_token: String,
    pub pow_secret: String,
    pub pow_difficulty: u32,
}

/// Configuration for Matrix connectivity.
/// Fields will be present or not depending on the `mode`.
#[derive(Debug, Deserialize)]
pub struct Matrix {
    pub mode: String, // "bot" or "appservice"
    pub homeserver_url: String,
    pub owner_id: String,

    // Bot mode fields
    pub user: Option<String>,
    pub token: Option<String>,
    pub device_id: Option<String>,
    // AppService mode fields are ignored for now
}

/// Reads configuration from `config.toml` and environment variables.
pub fn get_configuration() -> Result<Settings, config::ConfigError> {
    let settings = config::Config::builder()
        // Start with a config file named `config`
        .add_source(config::File::with_name("config").required(true))
        // Add in environment variables with a prefix of CUMMENTS and separator __
        // e.g. `CUMMENTS_SERVER__PORT=5000` would override `port` in `[server]`
        .add_source(config::Environment::with_prefix("CUMMENTS").separator("__"))
        .build()?;

    settings.try_deserialize()
}
