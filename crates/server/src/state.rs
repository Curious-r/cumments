use axum::extract::FromRef;
use domain::{AppCommand, IngestEvent};
use tokio::sync::{broadcast, mpsc};

use crate::pow::PowGuard;
use storage::Db;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub sender: mpsc::Sender<AppCommand>,
    pub tx_ingest: broadcast::Sender<IngestEvent>,
    pub pow: PowGuard,
}

impl FromRef<AppState> for Db {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}
