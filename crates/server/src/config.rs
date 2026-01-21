use config::ConfigError;
use serde::Deserialize;

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
}

#[derive(Deserialize, Clone)]
pub struct DatabaseSettings {
    pub url: String,
}

#[derive(Deserialize, Clone)]
pub struct SecuritySettings {
    pub global_salt: String,
}

#[derive(Deserialize, Clone)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum MatrixSettings {
    Bot {
        homeserver_url: String,
        user: String,
        token: String,
    },
    #[serde(rename = "appservice")]
    AppService {
        homeserver_url: String,
        server_name: String,
        as_token: String,
        hs_token: String,
        bot_localpart: String,
        listen_port: u16,
    },
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let run_mode = std::env::var("RUN_MODE").unwrap_or_else(|_| "development".into());

        let s = config::Config::builder()
            .set_default("server.host", "0.0.0.0")?
            .set_default("server.port", 3000)?
            .set_default("server.cors_origins", "*")?
            .set_default("database.url", "sqlite://data/cumments.db")?
            .set_default("matrix.mode", "bot")?
            .set_default("matrix.homeserver_url", "https://matrix.org")?
            .set_default("security.global_salt", "change_me_please")?
            .add_source(config::File::with_name("config").required(false))
            .add_source(config::File::with_name(&format!("config.{}", run_mode)).required(false))
            .add_source(
                config::Environment::with_prefix("CUMMENTS")
                    .separator("__")
                    .prefix_separator("_")
                    .try_parsing(true) // 尝试解析数字/布尔值
                    .convert_case(config::Case::Lower),
            )
            .build()?;

        s.try_deserialize()
    }
}
