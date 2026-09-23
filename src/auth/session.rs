use std::time::Duration;

use chrono::DateTime;
use chrono::TimeDelta;
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
}

#[derive(Debug, Clone)]
pub struct UserRow {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub timezone: String,
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
    let token_hash = hash_token(&token);
    let secs = i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX / 4_000);
    let delta = TimeDelta::try_seconds(secs)
        .ok_or_else(|| sqlx::Error::Configuration("session ttl is out of range".into()))?;
    let now = Utc::now();
    let expires_at = (now + delta).to_rfc3339();
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, expires_at, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&token_hash)
    .bind(user_id)
    .bind(expires_at)
    .bind(now.to_rfc3339())
    .execute(pool)
    .await?;
    return Ok(token);
}

pub async fn find_user_by_token(
    pool: &SqlitePool,
    token: &str,
) -> Result<Option<SessionUser>, sqlx::Error> {
    let token_hash = hash_token(token);
    let row = sqlx::query_as::<_, (i64, String, String, String)>(
        "SELECT u.id, u.username, u.timezone, s.expires_at \
         FROM sessions s JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = ?",
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await?;
    let Some((id, username, timezone, expires_at)) = row else {
        return Ok(None);
    };
    let expired = match DateTime::parse_from_rfc3339(&expires_at) {
        Ok(expires) => expires.with_timezone(&Utc) <= Utc::now(),
        Err(_) => true,
    };
    if expired {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(&token_hash)
            .execute(pool)
            .await?;
        return Ok(None);
    }
    return Ok(Some(SessionUser {
        id,
        username,
        timezone,
    }));
}

pub async fn delete(pool: &SqlitePool, token: &str) -> Result<(), sqlx::Error> {
    let token_hash = hash_token(token);
    sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
        .bind(&token_hash)
        .execute(pool)
        .await?;
    return Ok(());
}

pub async fn delete_other_sessions(
    pool: &SqlitePool,
    user_id: i64,
    keep_token: &str,
) -> Result<(), sqlx::Error> {
    let keep_hash = hash_token(keep_token);
    sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token_hash != ?")
        .bind(user_id)
        .bind(&keep_hash)
        .execute(pool)
        .await?;
    return Ok(());
}

pub async fn password_hash_by_id(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as("SELECT password_hash FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    return Ok(row.map(|(hash,)| hash));
}

pub async fn find_user_by_username(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<UserRow>, sqlx::Error> {
    let row = sqlx::query_as::<_, (i64, String, String, String)>(
        "SELECT id, username, password_hash, timezone FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    return Ok(row.map(|(id, username, password_hash, timezone)| UserRow {
        id,
        username,
        password_hash,
        timezone,
    }));
}

pub async fn insert_user(
    pool: &SqlitePool,
    username: &str,
    password_hash: &str,
    timezone: &str,
) -> Result<i64, sqlx::Error> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO users (username, password_hash, timezone, created_at) VALUES (?, ?, ?, ?) RETURNING id",
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
