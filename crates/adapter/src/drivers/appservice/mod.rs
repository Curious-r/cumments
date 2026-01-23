mod driver;
mod handlers;
mod utils;
mod web;

pub use driver::AppServiceDriver;

use crate::AppServiceConfig;
use matrix_sdk::Client;
use storage::Db;
use tokio::sync::broadcast;
use domain::IngestEvent;

/// AppService 运行时的共享上下文
#[derive(Clone)]
pub struct AsContext {
    pub db: Db,
    pub tx_ingest: broadcast::Sender<IngestEvent>,
    pub config: AppServiceConfig,
    // AS 的主 Bot Client，用于查询 Profile 或操作房间
    pub main_client: Client,
}
