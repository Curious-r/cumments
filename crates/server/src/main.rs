mod pow;
use anyhow::Context;
use axum::{
    extract::{Path, State},
    http::Method,
    routing::{get, post},
    Json, Router,
};
use dotenvy::dotenv;
use matrix_sdk::ruma::{EventId, UserId};
use serde::Deserialize;
use std::fmt::Display;
use std::str::FromStr;
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};

use domain::{AppCommand, SiteId};
use pow::PowGuard;
use storage::Db;

// --- Data Transfer Object ---
#[derive(Deserialize)]
pub struct CreateCommentRequest {
    pub post_slug: String,
    pub content: String,
    pub nickname: String,
    pub challenge_response: String,
    pub reply_to: Option<String>,
}

const APP_PREFIX: &str = "CUMMENTS_";

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

struct AppConfig {
    db_url: String,
    matrix: adapter::MatrixConfig,
    host: String,
    port: u16,
}

impl AppConfig {
    fn from_env() -> anyhow::Result<Self> {
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

#[derive(Clone)]
struct AppState {
    db: Db,
    sender: mpsc::Sender<AppCommand>,
    pow: PowGuard,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    tracing_subscriber::fmt::init();

    let config = AppConfig::from_env()?;

    let db = Db::new(&config.db_url).await?;

    let (tx, rx) = mpsc::channel(100);
    let db_for_worker = db.clone();
    tokio::spawn(async move {
        if let Err(e) = adapter::start(config.matrix, db_for_worker, rx).await {
            tracing::error!("Matrix worker crashed: {:?}", e);
        }
    });

    let pow = PowGuard::new();
    let state = AppState {
        db,
        sender: tx,
        pow,
    };

    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_origin(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/api/:site_id/comments/:slug", get(list_comments))
        .route("/api/:site_id/comments", post(post_comment))
        .route("/api/challenge", get(get_challenge))
        .layer(cors)
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port);
    info!("Server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to address: {}", addr))?;
    axum::serve(listener, app).await?;
    Ok(())
}

// --- Handlers ---
async fn get_challenge(State(state): State<AppState>) -> Json<serde_json::Value> {
    let secret = state.pow.generate_challenge();
    Json(serde_json::json!({ "secret": secret, "difficulty": 4 }))
}

async fn list_comments(
    State(state): State<AppState>,
    Path((site_id_str, slug)): Path<(String, String)>,
) -> Result<Json<Vec<domain::Comment>>, (axum::http::StatusCode, String)> {
    if SiteId::new(&site_id_str).is_err() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "Invalid Site ID format".to_string(),
        ));
    }

    let comments = state
        .db
        .list_comments(&site_id_str, &slug)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(comments))
}

async fn post_comment(
    State(state): State<AppState>,
    Path(site_id_str): Path<String>,
    Json(payload): Json<CreateCommentRequest>,
) -> Result<Json<&'static str>, (axum::http::StatusCode, String)> {
    let site_id = SiteId::new(site_id_str).map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e))?;

    if let Some(ref reply_id) = payload.reply_to {
        if EventId::parse(reply_id).is_err() {
            return Err((
                axum::http::StatusCode::BAD_REQUEST,
                format!("Invalid reply_to ID format: {}", reply_id),
            ));
        }
    }

    let parts: Vec<&str> = payload.challenge_response.split('|').collect();
    if parts.len() != 2 || !state.pow.verify(parts[0], parts[1]) {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            "Invalid PoW Challenge".to_string(),
        ));
    }

    let cmd = AppCommand::SendComment {
        site_id,
        post_slug: payload.post_slug,
        content: payload.content,
        nickname: payload.nickname,
        reply_to: payload.reply_to,
    };

    if state.sender.send(cmd).await.is_err() {
        return Err((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Worker closed".to_string(),
        ));
    }
    Ok(Json("Accepted"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_env_generics() {
        std::env::set_var("CUMMENTS_TEST_INT", "42");
        let val: u16 = get_env("TEST_INT", 0);
        assert_eq!(val, 42);

        std::env::set_var("TEST_BAD", "abc");
        let val: u16 = get_env("TEST_BAD", 100);
        assert_eq!(val, 100);

        std::env::remove_var("CUMMENTS_TEST_INT");
        std::env::remove_var("TEST_BAD");
    }
}
