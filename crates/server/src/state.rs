use crate::pow::PowGuard;
use adapter::MatrixSvc;
use axum::extract::FromRef;
use domain::IngestEvent;
use storage::Db;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub matrix: MatrixSvc,
    pub tx_ingest: broadcast::Sender<IngestEvent>,
    pub pow: PowGuard,
    pub admin_token: String,
    pub server_name: String,
    pub identity_salt: String,
}

impl FromRef<AppState> for Db {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}
