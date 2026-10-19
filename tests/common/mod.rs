use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use routinely::config::Config;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    return format!("t_{}_{nanos}_{seq}", std::process::id());
}

pub async fn config(signup_code: &str, session_ttl: Duration) -> Config {
    let path = std::env::temp_dir().join(format!("routinely_{}.db", unique_name()));
    return Config {
        database_path: path.to_string_lossy().into_owned(),
        bind: "127.0.0.1:0".parse().unwrap(),
        signup_code: signup_code.to_string(),
        session_ttl,
        cookie_secure: false,
    };
}
