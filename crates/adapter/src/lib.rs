mod common;
mod config;
mod drivers;
mod traits;

pub use crate::common::matrix_utils::SpaceCache;
pub use config::{AppServiceConfig, BotConfig, MatrixConfig};
pub use traits::MatrixDriver;

pub use self::facade::MatrixSvc;

use domain::{AppCommand, IngestEvent};
use drivers::appservice::AppServiceDriver;
use drivers::bot::BotDriver;
use storage::Db;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::info;

mod facade {
    use super::*;
    use anyhow::Result;

    pub struct CommandEnvelope {
        pub cmd: AppCommand,
        pub resp: oneshot::Sender<Result<()>>,
    }

    #[derive(Clone)]
    pub struct MatrixSvc {
        sender: mpsc::Sender<CommandEnvelope>,
    }

    impl MatrixSvc {
        pub fn new(sender: mpsc::Sender<CommandEnvelope>) -> Self {
            Self { sender }
        }

        pub async fn send(&self, cmd: AppCommand) -> Result<()> {
            let (tx, rx) = oneshot::channel();
            let envelope = CommandEnvelope { cmd, resp: tx };

            self.sender
                .send(envelope)
                .await
                .map_err(|_| anyhow::anyhow!("Matrix actor is closed"))?;

            let result = tokio::time::timeout(std::time::Duration::from_secs(10), rx)
                .await
                .map_err(|_| anyhow::anyhow!("Matrix actor timed out"))??;

            result
        }
    }
}

use self::facade::CommandEnvelope;

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
