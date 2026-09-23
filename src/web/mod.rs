pub mod day;
pub mod leaderboard;
pub mod notes;
pub mod occurrences;
pub mod punishments;
pub mod routines;
pub mod settings;
pub mod shell;
pub mod todos;
pub mod week;

pub use shell::AvatarVm;
pub use shell::Shell;

use axum::Router;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header;
use axum::middleware;
use axum::response::IntoResponse;
use axum::response::Response;
use thiserror::Error;

use crate::app::AppState;
use crate::auth::extract::token_from_header_value;
use crate::auth::session;

pub fn router() -> Router<AppState> {
    return Router::new()
        .merge(day::router())
        .merge(leaderboard::router())
        .merge(routines::router())
        .merge(occurrences::router())
        .merge(punishments::router())
        .merge(settings::router())
        .merge(todos::router())
        .merge(notes::router())
        .merge(week::router())
        .layer(middleware::from_fn(shell::clear_flash));
}

#[derive(Debug, Error)]
pub enum WebError {
    #[error("not found")]
    NotFound,
    #[error("conflict")]
    Conflict,
    #[error("{0}")]
    Validation(&'static str),
    #[error("internal")]
    Internal,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Template(#[from] askama::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Multipart(#[from] axum::extract::multipart::MultipartError),
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let status = match &self {
            WebError::NotFound => StatusCode::NOT_FOUND,
            WebError::Conflict => StatusCode::CONFLICT,
            WebError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            WebError::Multipart(e) => e.status(),
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let code = match &self {
            WebError::NotFound => "not_found",
            WebError::Conflict => "conflict",
            WebError::Validation(_) => "validation",
            WebError::Multipart(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                "payload_too_large"
            }
            WebError::Multipart(_) => "bad_request",
            _ => "internal",
        };
        return (status, axum::Json(serde_json::json!({ "error": code }))).into_response();
    }
}

pub type WebResult = Result<Response, WebError>;

pub async fn current_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<session::SessionUser>, sqlx::Error> {
    let Some(value) = headers.get(header::COOKIE) else {
        return Ok(None);
    };
    let Some(token) = token_from_header_value(value) else {
        return Ok(None);
    };
    return session::find_user_by_token(&state.pool, &token).await;
}

pub fn is_htmx(headers: &HeaderMap) -> bool {
    return headers
        .get("hx-request")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "true");
}

pub fn html<T: askama::Template>(template: &T) -> Result<Response, askama::Error> {
    let body = template.render()?;
    return Ok(axum::response::Html(body).into_response());
}

pub fn parse_date_param(
    raw: Option<&str>,
    today: chrono::NaiveDate,
) -> Result<chrono::NaiveDate, WebError> {
    let Some(raw) = raw else {
        return Ok(today);
    };
    return chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map_err(|_| WebError::Validation("invalid date"));
}

pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    return out;
}
