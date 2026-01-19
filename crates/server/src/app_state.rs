use axum::extract::FromRef;
use domain::{AppCommand, IngestEvent};
use std::fmt::Display;
use std::str::FromStr;
use tokio::sync::{broadcast, mpsc};
use tracing::warn;

use crate::pow::PowGuard;

use storage::Db;

const APP_PREFIX: &str = "CUMMENTS_";

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

pub struct AppConfig {
    pub db_url: String,
    pub matrix: adapter::MatrixConfig,
    pub host: String,
    pub port: u16,
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        use anyhow::Context;
        use matrix_sdk::ruma::UserId;

        let db_url: String = get_env("DATABASE_URL", "sqlite://data/cumments.db".to_string());

        let username_str: String = require_env("MATRIX_USER")?;
        let user_id = UserId::parse(&username_str)
            .with_context(|| format!("Invalid Matrix User ID format: {}", username_str))?;

        let matrix = adapter::MatrixConfig {
            homeserver_url: require_env("MATRIX_HOMESERVER")?,
            user_id,
            access_token: require_env("MATRIX_TOKEN")?,
        };

        let host: String = get_env("HOST", "0.0.0.0".to_string());
        let port: u16 = get_env("PORT", 3000);

        Ok(Self {
            db_url,
            matrix,
            host,
            port,
        })
    }
}

fn get_env<T>(key: &str, default: T) -> T
where
    T: FromStr + Display,
    <T as FromStr>::Err: Display,
{
    let prefixed_key = format!("{}{}", APP_PREFIX, key);
    let raw_value = match std::env::var(&prefixed_key).or_else(|_| std::env::var(key)) {
        Ok(v) => v,
        Err(_) => return default,
    };

    match raw_value.parse::<T>() {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Failed to parse env var '{}={}'. Error: {}. Using default: {}",
                key, raw_value, e, default
            );
            default
        }
    }
}

fn require_env<T>(key: &str) -> anyhow::Result<T>
where
    T: FromStr,
    <T as FromStr>::Err: std::error::Error + Send + Sync + 'static,
{
    let prefixed_key = format!("{}{}", APP_PREFIX, key);
    let raw_value = std::env::var(&prefixed_key)
        .or_else(|_| std::env::var(key))
        .map_err(|_| anyhow::anyhow!("Env missing: {} or {}", prefixed_key, key))?;

    raw_value.parse::<T>().map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse env var '{}={}'. Error: {}",
            key,
            raw_value,
            e
        )
    })
}
