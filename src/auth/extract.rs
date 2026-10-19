use axum::Json;
use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use axum::response::Redirect;
use axum::response::Response;
use sqlx::PgPool;
use thiserror::Error;

use crate::app::AppState;
use crate::auth::session::SessionUser;
use crate::cookie;
use crate::punish;
use crate::store;

use super::SESSION_COOKIE_NAME;
use super::TZ_COOKIE_NAME;
use super::session;

pub struct ApiAuth(pub session::SessionUser);

pub struct PageAuth(pub session::SessionUser);

#[derive(Debug, Error)]
pub enum ApiAuthError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("internal server error")]
    Internal,
}

#[derive(Debug, Error)]
pub enum PageAuthError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("internal server error")]
    Internal,
}

impl IntoResponse for ApiAuthError {
    fn into_response(self) -> Response {
        return match self {
            ApiAuthError::Unauthorized => unauthorized_response(),
            ApiAuthError::Internal => internal_response(),
        };
    }
}

impl IntoResponse for PageAuthError {
    fn into_response(self) -> Response {
        return match self {
            PageAuthError::Unauthorized => Redirect::to("/login").into_response(),
            PageAuthError::Internal => internal_response(),
        };
    }
}

impl FromRequestParts<AppState> for ApiAuth {
    type Rejection = ApiAuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = token_from_parts(parts).ok_or(ApiAuthError::Unauthorized)?;
        let user = session::find_user_by_token(&state.pool, &state.kv, &token)
            .await
            .map_err(|_| ApiAuthError::Internal)?;
        let Some(user) = user else {
            return Err(ApiAuthError::Unauthorized);
        };
        run_eval_hook(&state.pool, &user)
            .await
            .map_err(|_| ApiAuthError::Internal)?;
        return Ok(ApiAuth(user));
    }
}

impl FromRequestParts<AppState> for PageAuth {
    type Rejection = PageAuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = token_from_parts(parts).ok_or(PageAuthError::Unauthorized)?;
        let user = session::find_user_by_token(&state.pool, &state.kv, &token)
            .await
            .map_err(|_| PageAuthError::Internal)?;
        let Some(mut user) = user else {
            return Err(PageAuthError::Unauthorized);
        };
        let tz = cookie::get(&parts.headers, TZ_COOKIE_NAME).and_then(|raw| cookie::decode(&raw));
        super::sync_timezone(&state.pool, &mut user, tz.as_deref())
            .await
            .map_err(|_| PageAuthError::Internal)?;
        run_eval_hook(&state.pool, &user)
            .await
            .map_err(|_| PageAuthError::Internal)?;
        return Ok(PageAuth(user));
    }
}

async fn run_eval_hook(pool: &PgPool, user: &SessionUser) -> Result<(), sqlx::Error> {
    let today = store::user_today(&user.timezone);
    let today_str = today.to_string();
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT last_evaluated_day FROM users WHERE id = $1")
            .bind(user.id)
            .fetch_optional(pool)
            .await?;
    let Some((last_day,)) = row else {
        return Ok(());
    };
    let stale = match last_day {
        Some(day) => day.as_str() < today_str.as_str(),
        None => true,
    };
    if !stale {
        return Ok(());
    }
    return punish::evaluate_user(pool, user, today).await.map(|_| ());
}

pub fn token_from_parts(parts: &Parts) -> Option<String> {
    return token_from_header_value(parts.headers.get(header::COOKIE)?);
}

pub fn token_from_header_value(value: &axum::http::HeaderValue) -> Option<String> {
    let raw = value.to_str().ok()?;
    for pair in raw.split(';') {
        let pair = pair.trim();
        let Some((name, token)) = pair.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(SESSION_COOKIE_NAME) && !token.is_empty() {
            return Some(token.to_string());
        }
    }
    return None;
}

fn unauthorized_response() -> Response {
    return (
        axum::http::StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": "unauthorized" })),
    )
        .into_response();
}

fn internal_response() -> Response {
    return (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "internal" })),
    )
        .into_response();
}
