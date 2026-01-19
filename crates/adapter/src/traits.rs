use anyhow::Result;
use async_trait::async_trait;
use domain::{AppCommand, IngestEvent};
use storage::Db;
use tokio::sync::{broadcast, mpsc};

#[async_trait]
pub trait MatrixDriver: Send + Sync {
    async fn run(
        &self,
        db: Db,
        rx_cmd: mpsc::Receiver<AppCommand>,
        tx_ingest: broadcast::Sender<IngestEvent>,
    ) -> Result<()>;
}
