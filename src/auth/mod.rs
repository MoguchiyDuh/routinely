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
pub const TZ_COOKIE_NAME: &str = "tz";

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

#[derive(Debug, Error)]
pub enum PasswordChangeError {
    #[error("current password is incorrect")]
    WrongCurrent,
    #[error("new password must be 8-128 characters")]
    InvalidNew,
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
        day_start: 0,
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
        day_start: user.day_start,
    });
}

pub async fn change_password(
    pool: &SqlitePool,
    user_id: i64,
    current_token: &str,
    current_password: Option<&str>,
    new_password: Option<&str>,
) -> Result<(), PasswordChangeError> {
    let (Some(current), Some(next)) = (current_password, new_password) else {
        return Err(PasswordChangeError::InvalidNew);
    };
    let stored = session::password_hash_by_id(pool, user_id)
        .await?
        .ok_or(PasswordChangeError::WrongCurrent)?;
    let valid = password::verify(current, &stored)?;
    if !valid {
        return Err(PasswordChangeError::WrongCurrent);
    }
    let new_hash = password::hash(next).map_err(|err| match err {
        PasswordError::Length => PasswordChangeError::InvalidNew,
        other => PasswordChangeError::Hash(other),
    })?;
    sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
        .bind(&new_hash)
        .bind(user_id)
        .execute(pool)
        .await?;
    session::delete_other_sessions(pool, user_id, current_token).await?;
    return Ok(());
}

pub async fn sync_timezone(
    pool: &SqlitePool,
    user: &mut session::SessionUser,
    raw: Option<&str>,
) -> Result<(), sqlx::Error> {
    let Some(raw) = raw else {
        return Ok(());
    };
    let Ok(tz) = raw.trim().parse::<chrono_tz::Tz>() else {
        return Ok(());
    };
    let name = tz.name();
    if name == user.timezone {
        return Ok(());
    }
    sqlx::query("UPDATE users SET timezone = $1 WHERE id = $2")
        .bind(name)
        .bind(user.id)
        .execute(pool)
        .await?;
    user.timezone = name.to_string();
    return Ok(());
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
