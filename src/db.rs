use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use thiserror::Error;

use crate::config::Config;

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

pub async fn init(config: &Config) -> Result<PgPool, DbError> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&config.database_url)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    return Ok(pool);
}
