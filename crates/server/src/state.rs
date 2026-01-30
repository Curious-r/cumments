use crate::pow::PowGuard;
use adapter::CommandEnvelope;
use axum::extract::FromRef;
use domain::IngestEvent;
use storage::Db;
use tokio::sync::{broadcast, mpsc};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub sender: mpsc::Sender<CommandEnvelope>,
    pub tx_ingest: broadcast::Sender<IngestEvent>,
    pub pow: PowGuard,
    pub admin_token: String,
    pub server_name: String,
    pub pow_difficulty: u32,
    pub identity_salt: String,
}

impl FromRef<AppState> for Db {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}
