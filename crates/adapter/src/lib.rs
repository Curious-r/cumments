mod common;
mod drivers;
mod traits;

pub use common::matrix_utils::SpaceCache;
pub use drivers::bot::BotConfig;
pub use traits::MatrixDriver;

use domain::{AppCommand, IngestEvent};
use drivers::appservice::AppServiceDriver;
use drivers::bot::BotDriver;
use storage::Db;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::info;
use matrix_sdk::ruma::OwnedUserId;

// --- 信封模式核心定义 ---
pub struct CommandEnvelope {
    pub cmd: AppCommand,
    // 结果回传通道：API 层等待这个 Result
    pub resp: oneshot::Sender<anyhow::Result<()>>,
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
    pub owner_id: Option<OwnedUserId>, // 新增：双皇共治
}

#[derive(Clone)]
pub enum MatrixConfig {
    Bot(BotConfig),
    AppService(AppServiceConfig),
}

pub async fn start_with_cancel_token(
    config: MatrixConfig,
    db: Db,
    // 注意：这里接收的是信封
    rx: mpsc::Receiver<CommandEnvelope>,
    tx_ingest: broadcast::Sender<IngestEvent>,
    cancel_token: CancellationToken,
) -> anyhow::Result<()> {
    let driver: Box<dyn MatrixDriver> = match config {
        MatrixConfig::Bot(bot_conf) => {
            info!("Initializing Adapter in BOT mode...");
            Box::new(BotDriver::new(bot_conf))
        }
        MatrixConfig::AppService(as_conf) => {
            info!("Initializing Adapter in APP_SERVICE mode...");
            Box::new(AppServiceDriver::new(as_conf))
        }
    };

    driver.run(db, rx, tx_ingest, cancel_token).await
}
