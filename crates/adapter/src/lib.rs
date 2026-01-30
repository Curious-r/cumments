mod common;
mod config;
mod drivers;
mod traits;

pub use common::matrix_utils::SpaceCache;
pub use config::{AppServiceConfig, BotConfig, MatrixConfig};
pub use traits::MatrixDriver;

use domain::{AppCommand, IngestEvent};
use drivers::appservice::AppServiceDriver;
use drivers::bot::BotDriver;
use storage::Db;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::info;

pub struct CommandEnvelope {
    pub cmd: AppCommand,
    pub resp: oneshot::Sender<anyhow::Result<()>>,
}

pub async fn start_with_cancel_token(
    config: MatrixConfig,
    db: Db,
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
