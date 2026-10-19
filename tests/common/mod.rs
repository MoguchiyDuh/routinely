use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use sqlx::Connection;
use sqlx::PgConnection;

use routinely::config::Config;
use routinely::kv::Kv;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn admin_url() -> String {
    return std::env::var("ROUTINELY_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:55432/postgres".to_string());
}

fn valkey_url() -> String {
    return std::env::var("ROUTINELY_TEST_VALKEY_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:56379".to_string());
}

fn unique_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    return format!("t_{}_{nanos}_{seq}", std::process::id());
}

pub async fn config(signup_code: &str, session_ttl: Duration) -> Config {
    let name = unique_name();
    let admin = admin_url();
    let mut conn = PgConnection::connect(&admin).await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();
    let (base, _) = admin.rsplit_once('/').unwrap();
    return Config {
        database_url: format!("{base}/{name}"),
        valkey_url: valkey_url(),
        valkey_prefix: format!("{name}:"),
        bind: "127.0.0.1:0".parse().unwrap(),
        signup_code: signup_code.to_string(),
        session_ttl,
        cookie_secure: false,
    };
}

pub async fn kv(config: &Config) -> Kv {
    return Kv::connect(&config.valkey_url, &config.valkey_prefix)
        .await
        .unwrap();
}
