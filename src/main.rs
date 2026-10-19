use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::signal::unix::SignalKind;
use tokio::signal::unix::signal;
use tokio::time::timeout;

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
    #[error("healthcheck failed: {0}")]
    Health(&'static str),
}

#[tokio::main]
async fn main() -> Result<(), AppError> {
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        return healthcheck().await;
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let config = Config::from_env()?;
    let pool = db::init(&config).await?;
    std::fs::create_dir_all(routinely::store::UPLOADS_DIR)?;
    routinely::notify::spawn(pool.clone());
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

async fn healthcheck() -> Result<(), AppError> {
    let bind = match std::env::var("ROUTINELY_BIND") {
        Ok(value) => value
            .parse::<SocketAddr>()
            .map_err(|_| AppError::Health("invalid ROUTINELY_BIND"))?,
        Err(_) => SocketAddr::from(([127, 0, 0, 1], 8080)),
    };
    let ip = if bind.ip().is_unspecified() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        bind.ip()
    };
    let limit = Duration::from_secs(3);
    let mut stream = timeout(limit, TcpStream::connect(SocketAddr::new(ip, bind.port())))
        .await
        .map_err(|_| AppError::Health("connect timed out"))??;
    stream
        .write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .await?;
    let mut response = Vec::new();
    timeout(limit, stream.read_to_end(&mut response))
        .await
        .map_err(|_| AppError::Health("read timed out"))??;
    let status = response.split(|byte| *byte == b' ').nth(1);
    if status != Some(b"200".as_slice()) {
        return Err(AppError::Health("unhealthy response"));
    }
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
