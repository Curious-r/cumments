#[derive(Clone)]
pub struct BotConfig {
    pub homeserver_url: String,
    pub user_id: String,
    pub access_token: String,
    pub identity_salt: String,
    pub device_id: String,
    pub owner_id: Option<String>,
}

#[derive(Clone)]
pub struct AppServiceConfig {
    pub homeserver_url: String,
    pub server_name: String,
    pub as_token: String,
    pub hs_token: String,
    pub bot_localpart: String,
    pub listen_port: u16,
    pub identity_salt: String,
    pub owner_id: Option<String>,
}

#[derive(Clone)]
pub enum MatrixConfig {
    Bot(BotConfig),
    AppService(AppServiceConfig),
}
