use crate::CommandEnvelope;
use anyhow::Result;
use async_trait::async_trait;
use domain::IngestEvent;
use storage::Db;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait MatrixDriver: Send + Sync {
    async fn run(
        &self,
        db: Db,
        // 接收信封
        rx_cmd: mpsc::Receiver<CommandEnvelope>,
        tx_ingest: broadcast::Sender<IngestEvent>,
        cancel_token: CancellationToken,
    ) -> Result<()>;
}
