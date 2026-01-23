use axum::extract::FromRef;
use adapter::CommandEnvelope; // 引入信封
use domain::IngestEvent;
use tokio::sync::{broadcast, mpsc};
use crate::pow::PowGuard;
use storage::Db;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    // 修改：发送信封
    pub sender: mpsc::Sender<CommandEnvelope>,
    pub tx_ingest: broadcast::Sender<IngestEvent>,
    pub pow: PowGuard,
    pub admin_token: String,
    // 新增：用于生成 Deep Link
    pub server_name: String,
}

impl FromRef<AppState> for Db {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}
