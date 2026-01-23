use config::ConfigError;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Deserialize, Clone)]
pub struct Settings {
    pub server: ServerSettings,
    pub database: DatabaseSettings,
    pub matrix: MatrixSettings,
    pub security: SecuritySettings,
}

#[derive(Deserialize, Clone)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
    pub cors_origins: String,
    pub public_server_name: String,
}

#[derive(Deserialize, Clone)]
pub struct DatabaseSettings {
    pub url: String,
}

#[derive(Deserialize, Clone)]
pub struct SecuritySettings {
    pub identity_salt: String,
    pub admin_token: String,
    pub pow_secret: String,
}

#[derive(Deserialize, Clone)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum MatrixSettings {
    Bot {
        homeserver_url: String,
        user: String,
        token: String,
        device_id: Option<String>,
        owner_id: Option<String>,
    },
    #[serde(rename = "appservice")]
    AppService {
        homeserver_url: String,
        server_name: String,
        as_token: String,
        hs_token: String,
        bot_localpart: String,
        listen_port: u16,
        owner_id: Option<String>,
    },
}

impl Settings {
    pub fn new(custom_path: Option<String>) -> Result<Self, ConfigError> {
        let run_mode = std::env::var("RUN_MODE").unwrap_or_else(|_| "development".to_string());
        let env_map = collect_env_vars();

        let mut builder = config::Config::builder()
            .set_default("server.host", "0.0.0.0")?
            .set_default("server.port", 3000)?
            .set_default("server.cors_origins", "*")?
            .set_default("server.public_server_name", "matrix.org")?
            .set_default("database.url", "sqlite://data/cumments.db")?
            .set_default("matrix.mode", "bot")?
            .set_default("matrix.homeserver_url", "https://matrix.org")?
            .set_default("security.identity_salt", "change_me_please")?
            .set_default("security.admin_token", "admin_secret_123")?
            .set_default("security.pow_secret", "pow_secret_change_me")?
            .add_source(config::File::with_name("config").required(false))
            .add_source(config::File::with_name(&format!("config.{}", run_mode)).required(false));

        if let Some(path) = custom_path {
            builder = builder.add_source(config::File::from(Path::new(&path)).required(true));
        }

        builder = builder.add_source(config::File::from_str(
            &serde_json::to_string(&env_map).map_err(|e| ConfigError::Message(e.to_string()))?,
            config::FileFormat::Json,
        ));

        builder.build()?.try_deserialize()
    }
}

fn collect_env_vars() -> HashMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| k.starts_with("CUMMENTS_"))
        .map(|(k, v)| {
            let new_key = k
                .trim_start_matches("CUMMENTS_")
                .replace("__", ".")
                .to_lowercase();
            (new_key, v)
        })
        .collect()
}
