use std::net::SocketAddr;

use tokio::signal::unix::SignalKind;
use tokio::signal::unix::signal;

use thiserror::Error;
use tracing_subscriber::EnvFilter;

use routinely::app::{self, AppState};
use routinely::config::{Config, ConfigError};
use routinely::db::{self, DbError};

#[derive(Debug, Error)]
enum AppError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("failed to bind {addr}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

#[tokio::main]
async fn main() -> Result<(), AppError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let config = Config::from_env()?;
    let pool = db::init(&config).await?;
    std::fs::create_dir_all(routinely::store::UPLOADS_DIR)?;
    let state = AppState {
        pool,
        config: config.clone(),
    };
    let router = app::build_router(state);
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .map_err(|source| AppError::Bind {
            addr: config.bind,
            source,
        })?;
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    return Ok(());
}

async fn shutdown_signal() {
    let Ok(mut terminate) = signal(SignalKind::terminate()) else {
        let _ = tokio::signal::ctrl_c().await;
        return;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}
