mod pow;
use axum::{
    extract::{Path, State},
    http::Method,
    routing::{get, post},
    Json, Router,
};
use domain::{MatrixCommand, PostCommentCmd};
use dotenvy::dotenv;
use pow::PowGuard;
use storage::Db;
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};

use anyhow::Context;
use matrix_sdk::ruma::UserId;
use std::fmt::Display;
use std::str::FromStr;

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
            .with_context(|| format!("Invalid Matrix User ID: {}", username_str))?;

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
    sender: mpsc::Sender<MatrixCommand>,
    pow: PowGuard,
}

fn validate_site_id(site_id: &str) -> Result<(), &'static str> {
    if site_id.contains('_') {
        return Err(
            "Site ID cannot contain underscores ('_'). Please use hyphens ('-') or dots ('.') instead.",
        );
    }
    if !site_id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
    {
        return Err("Site ID contains invalid characters. Only lowercase alphanumeric, '.', and '-' are allowed.");
    }
    if site_id.len() > 64 {
        return Err("Site ID is too long (max 64 chars).");
    }
    Ok(())
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
        .with_context(|| format!("Failed to bind to {}", addr))?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn get_challenge(State(state): State<AppState>) -> Json<serde_json::Value> {
    let secret = state.pow.generate_challenge();
    Json(serde_json::json!({ "secret": secret, "difficulty": 4 }))
}

async fn list_comments(
    State(state): State<AppState>,
    Path((site_id, slug)): Path<(String, String)>,
) -> Result<Json<Vec<domain::Comment>>, (axum::http::StatusCode, String)> {
    if let Err(e) = validate_site_id(&site_id) {
        return Err((axum::http::StatusCode::BAD_REQUEST, e.to_string()));
    }

    let comments = state
        .db
        .list_comments(&site_id, &slug)
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(comments))
}

async fn post_comment(
    State(state): State<AppState>,
    Path(site_id): Path<String>,
    Json(payload): Json<PostCommentCmd>,
) -> Result<Json<&'static str>, (axum::http::StatusCode, String)> {
    if let Err(e) = validate_site_id(&site_id) {
        return Err((axum::http::StatusCode::BAD_REQUEST, e.to_string()));
    }

    let parts: Vec<&str> = payload.challenge_response.split('|').collect();
    if parts.len() != 2 || !state.pow.verify(parts[0], parts[1]) {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            "Invalid PoW Challenge".to_string(),
        ));
    }

    let content_formatted = format!("**{}** (Guest): {}", payload.nickname, payload.content);

    let cmd = MatrixCommand::SendComment {
        site_id,
        post_slug: payload.post_slug,
        content: content_formatted,
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
        let val: u16 = get_env("TEST_BAD", 100); // Should warn and return default
        assert_eq!(val, 100);

        std::env::remove_var("CUMMENTS_TEST_INT");
        std::env::remove_var("TEST_BAD");
    }

    #[test]
    fn test_validate_site_id() {
        assert!(validate_site_id("example.com").is_ok());
        assert!(validate_site_id("my_blog").is_err()); // Underscore
        assert!(validate_site_id("MyBlog").is_err()); // Uppercase
    }
}
