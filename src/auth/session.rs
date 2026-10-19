use std::time::Duration;

use chrono::Utc;
use rand::RngExt;
use rand::rng;
use sha2::Digest;
use sha2::Sha256;
use sqlx::PgPool;

use crate::kv::Kv;
use crate::kv::KvError;

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

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Kv(#[from] KvError),
}

pub async fn create(kv: &Kv, user_id: i64, ttl: Duration) -> Result<String, KvError> {
    let token = new_token();
    kv.session_put(&hash_token(&token), user_id, ttl).await?;
    return Ok(token);
}

pub async fn find_user_by_token(
    pool: &PgPool,
    kv: &Kv,
    token: &str,
) -> Result<Option<SessionUser>, SessionError> {
    let token_hash = hash_token(token);
    let Some(user_id) = kv.session_user(&token_hash).await? else {
        return Ok(None);
    };
    let row = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT id, username, timezone FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    let Some((id, username, timezone)) = row else {
        kv.session_delete(&token_hash).await?;
        return Ok(None);
    };
    return Ok(Some(SessionUser {
        id,
        username,
        timezone,
    }));
}

pub async fn delete(kv: &Kv, token: &str) -> Result<(), KvError> {
    return kv.session_delete(&hash_token(token)).await;
}

pub async fn delete_other_sessions(kv: &Kv, user_id: i64, keep_token: &str) -> Result<(), KvError> {
    return kv
        .session_delete_others(user_id, &hash_token(keep_token))
        .await;
}

pub async fn password_hash_by_id(
    pool: &PgPool,
    user_id: i64,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as("SELECT password_hash FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    return Ok(row.map(|(hash,)| hash));
}

pub async fn find_user_by_username(
    pool: &PgPool,
    username: &str,
) -> Result<Option<UserRow>, sqlx::Error> {
    let row = sqlx::query_as::<_, (i64, String, String, String)>(
        "SELECT id, username, password_hash, timezone FROM users WHERE lower(username) = lower($1)",
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
    pool: &PgPool,
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
