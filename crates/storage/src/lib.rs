use sqlx::{migrate::MigrateDatabase, sqlite::SqlitePoolOptions, Pool, Sqlite};
use std::{fs, path::Path};
mod models;
mod repo;
#[derive(Clone)]
pub struct Db {
    pub(crate) pool: Pool<Sqlite>,
}
impl Db {
    pub async fn new(db_url: &str) -> anyhow::Result<Self> {
        if db_url.starts_with("sqlite://") && !db_url.contains(":memory:") {
            let path_str = db_url.trim_start_matches("sqlite://");
            let path = Path::new(path_str);
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() && !parent.exists() {
                    fs::create_dir_all(parent)?;
                }
            }
        }
        if !Sqlite::database_exists(db_url).await.unwrap_or(false) {
            Sqlite::create_database(db_url).await?;
        }
        let pool = SqlitePoolOptions::new().connect(db_url).await?;
        sqlx::query("PRAGMA journal_mode = WAL;")
            .execute(&pool)
            .await?;
        sqlx::query("PRAGMA synchronous = NORMAL;")
            .execute(&pool)
            .await?;
        sqlx::migrate!("../../migrations").run(&pool).await?;
        Ok(Self { pool })
    }
}
