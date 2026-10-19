use std::time::Duration;

use chrono::Utc;
use rand::RngExt;
use rand::rng;
use sha2::Digest;
use sha2::Sha256;
use sqlx::SqlitePool;

use super::SESSION_COOKIE_NAME;

const TOKEN_BYTES: usize = 32;

#[derive(Debug, Clone)]
pub struct SessionUser {
    pub id: i64,
    pub username: String,
    pub timezone: String,
    pub day_start: u32,
}

#[derive(Debug, Clone)]
pub struct UserRow {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub timezone: String,
    pub day_start: u32,
}

pub fn new_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rng().fill(&mut bytes);
    return hex_encode(&bytes);
}

pub fn hash_token(token: &str) -> String {
    return hex_encode(&Sha256::digest(token.as_bytes()));
}

pub fn build_cookie(token: &str, secure: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/");
    if secure {
        cookie.push_str("; Secure");
    }
    return cookie;
}

pub fn clear_cookie() -> String {
    return format!("{SESSION_COOKIE_NAME}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0");
}

pub async fn create(pool: &SqlitePool, user_id: i64, ttl: Duration) -> Result<String, sqlx::Error> {
    let token = new_token();
    let now = Utc::now().timestamp();
    let expires_at = now.saturating_add(i64::try_from(ttl.as_secs().max(1)).unwrap_or(i64::MAX));
    sqlx::query("DELETE FROM sessions WHERE expires_at <= $1")
        .bind(now)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, $3)")
        .bind(hash_token(&token))
        .bind(user_id)
        .bind(expires_at)
        .execute(pool)
        .await?;
    return Ok(token);
}

pub async fn find_user_by_token(
    pool: &SqlitePool,
    token: &str,
) -> Result<Option<SessionUser>, sqlx::Error> {
    let row = sqlx::query_as::<_, (i64, String, String, i64)>(
        "SELECT u.id, u.username, u.timezone, u.day_start_hour FROM sessions s JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = $1 AND s.expires_at > $2",
    )
    .bind(hash_token(token))
    .bind(Utc::now().timestamp())
    .fetch_optional(pool)
    .await?;
    return Ok(row.map(|(id, username, timezone, day_start)| SessionUser {
        id,
        username,
        timezone,
        day_start: crate::store::day_start_hour(day_start),
    }));
}

pub async fn delete(pool: &SqlitePool, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(hash_token(token))
        .execute(pool)
        .await?;
    return Ok(());
}

pub async fn delete_other_sessions(
    pool: &SqlitePool,
    user_id: i64,
    keep_token: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND token_hash <> $2")
        .bind(user_id)
        .bind(hash_token(keep_token))
        .execute(pool)
        .await?;
    return Ok(());
}

pub async fn session_count(pool: &SqlitePool, user_id: i64) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE user_id = $1 AND expires_at > $2")
            .bind(user_id)
            .bind(Utc::now().timestamp())
            .fetch_one(pool)
            .await?;
    return Ok(count);
}

pub async fn password_hash_by_id(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as("SELECT password_hash FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    return Ok(row.map(|(hash,)| hash));
}

pub async fn find_user_by_username(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<UserRow>, sqlx::Error> {
    let row = sqlx::query_as::<_, (i64, String, String, String, i64)>(
        "SELECT id, username, password_hash, timezone, day_start_hour FROM users WHERE lower(username) = lower($1)",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    return Ok(row.map(
        |(id, username, password_hash, timezone, day_start)| UserRow {
            id,
            username,
            password_hash,
            timezone,
            day_start: crate::store::day_start_hour(day_start),
        },
    ));
}

pub async fn insert_user(
    pool: &SqlitePool,
    username: &str,
    password_hash: &str,
    timezone: &str,
) -> Result<i64, sqlx::Error> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO users (username, password_hash, timezone, created_at) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(username)
    .bind(password_hash)
    .bind(timezone)
    .bind(Utc::now().to_rfc3339())
    .fetch_one(pool)
    .await?;
    return Ok(id);
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    return out;
}
