use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use routinely::app::{self, AppState};
use routinely::config::Config;
use routinely::db;

type Server = (SocketAddr, SqlitePool, PathBuf, tokio::task::JoinHandle<()>);

async fn setup(name: &str) -> Server {
    let unique = format!(
        "routinely-smoke-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let dir = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&dir).unwrap();
    let config = Config {
        db_path: dir.join("smoke.db"),
        bind: "127.0.0.1:0".parse().unwrap(),
        signup_code: "smoke-code".to_string(),
        session_ttl: Duration::from_secs(60),
        cookie_secure: false,
    };
    let pool = db::init(&config).await.unwrap();
    let state = AppState {
        pool: pool.clone(),
        config: config.clone(),
    };
    let router = app::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    return (addr, pool, dir, server);
}

async fn get(addr: SocketAddr, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: smoke.test\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    return String::from_utf8(raw).unwrap();
}

#[tokio::test]
async fn migrations_and_healthz() {
    let (addr, pool, dir, server) = setup("migrations").await;

    for table in ["users", "routines", "punishments"] {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?")
                .bind(table)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1, "table {table} missing");
    }

    let response = get(addr, "/healthz").await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "unexpected status: {response}"
    );
    assert!(response.ends_with("ok"), "unexpected body: {response}");

    server.abort();
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn static_css_served() {
    let (addr, pool, dir, server) = setup("static-css").await;
    let response = get(addr, "/static/app.css").await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "css request failed: {response}"
    );
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(
        head.to_ascii_lowercase().contains("content-type: text/css"),
        "unexpected content type: {head}"
    );
    assert!(!body.is_empty(), "css body must not be empty");
    assert!(body.contains(":root"), "css tokens missing: {body}");

    server.abort();
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}
