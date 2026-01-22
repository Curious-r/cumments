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
use tokio::sync::{broadcast, mpsc};
use tracing::info;

#[derive(Clone)]
pub struct AppServiceConfig {
    pub homeserver_url: String,
    pub server_name: String,
    pub as_token: String,
    pub hs_token: String,
    pub bot_localpart: String,
    pub listen_port: u16,

    pub identity_salt: String,
}

#[derive(Clone)]
pub enum MatrixConfig {
    Bot(BotConfig),
    AppService(AppServiceConfig),
}

pub async fn start(
    config: MatrixConfig,
    db: Db,
    rx: mpsc::Receiver<AppCommand>,
    tx_ingest: broadcast::Sender<IngestEvent>,
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

    driver.run(db, rx, tx_ingest).await
}
