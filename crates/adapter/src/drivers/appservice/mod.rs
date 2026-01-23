mod driver;
mod handlers;
mod utils;
mod web;

pub use driver::AppServiceDriver;

use crate::AppServiceConfig;
use domain::IngestEvent;
use matrix_sdk::Client;
use storage::Db;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct AsContext {
    pub db: Db,
    pub tx_ingest: broadcast::Sender<IngestEvent>,
    pub config: AppServiceConfig,
    pub main_client: Client,
}
