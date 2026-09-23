pub mod extract;
pub mod password;
pub mod routes;
pub mod session;

use sqlx::SqlitePool;
use thiserror::Error;

use crate::config::Config;
use crate::punish;

use self::password::PasswordError;

pub const SESSION_COOKIE_NAME: &str = "session";

pub const MIN_USERNAME_CHARS: usize = 3;
pub const MAX_USERNAME_CHARS: usize = 32;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("signup code mismatch")]
    Forbidden,
    #[error("username already taken")]
    UsernameTaken,
    #[error("invalid username")]
    InvalidUsername,
    #[error("invalid password")]
    InvalidPassword,
    #[error("invalid timezone")]
    InvalidTimezone,
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error(transparent)]
    Hash(#[from] PasswordError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub async fn perform_signup(
    pool: &SqlitePool,
    config: &Config,
    username: Option<String>,
    password: Option<String>,
    timezone: Option<String>,
    code: Option<String>,
) -> Result<session::SessionUser, AuthError> {
    let Some(provided) = code else {
        return Err(AuthError::Forbidden);
    };
    if provided != config.signup_code {
        return Err(AuthError::Forbidden);
    }
    let Some(username) = username else {
        return Err(AuthError::InvalidUsername);
    };
    if !is_valid_username(&username) {
        return Err(AuthError::InvalidUsername);
    }
    let Some(password) = password else {
        return Err(AuthError::InvalidPassword);
    };
    let bytes = password.as_bytes();
    if bytes.len() < password::MIN_PASSWORD_BYTES || bytes.len() > password::MAX_PASSWORD_BYTES {
        return Err(AuthError::InvalidPassword);
    }
    let timezone = resolve_timezone(timezone)?;
    if session::find_user_by_username(pool, &username)
        .await?
        .is_some()
    {
        return Err(AuthError::UsernameTaken);
    }
    let hash = password::hash(&password)?;
    let inserted = session::insert_user(pool, &username, &hash, &timezone).await;
    let id = match inserted {
        Ok(id) => id,
        Err(err) => {
            if is_unique_violation(&err) {
                return Err(AuthError::UsernameTaken);
            }
            return Err(AuthError::Db(err));
        }
    };
    punish::seed_catalog_if_empty(pool, id).await?;
    return Ok(session::SessionUser {
        id,
        username,
        timezone,
    });
}

pub async fn perform_login(
    pool: &SqlitePool,
    username: Option<String>,
    password: Option<String>,
) -> Result<session::SessionUser, AuthError> {
    let Some(username) = username else {
        return Err(AuthError::InvalidCredentials);
    };
    let Some(password) = password else {
        return Err(AuthError::InvalidCredentials);
    };
    let user = session::find_user_by_username(pool, &username).await?;
    let Some(user) = user else {
        return Err(AuthError::InvalidCredentials);
    };
    let valid = password::verify(&password, &user.password_hash)?;
    if !valid {
        return Err(AuthError::InvalidCredentials);
    }
    return Ok(session::SessionUser {
        id: user.id,
        username: user.username,
        timezone: user.timezone,
    });
}

fn is_valid_username(username: &str) -> bool {
    let len = username.chars().count();
    if !(MIN_USERNAME_CHARS..=MAX_USERNAME_CHARS).contains(&len) {
        return false;
    }
    return username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
}

fn resolve_timezone(timezone: Option<String>) -> Result<String, AuthError> {
    let Some(raw) = timezone else {
        return Ok("UTC".to_string());
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok("UTC".to_string());
    }
    let parsed = trimmed
        .parse::<chrono_tz::Tz>()
        .map_err(|_| AuthError::InvalidTimezone)?;
    return Ok(parsed.name().to_string());
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    return match err {
        sqlx::Error::Database(db) => db.is_unique_violation(),
        _ => false,
    };
}
