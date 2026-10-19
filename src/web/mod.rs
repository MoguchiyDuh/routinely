pub mod day;
pub mod journal;
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

use askama::Template;
use axum::Router;
use axum::extract::Request;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header;
use axum::middleware;
use axum::middleware::Next;
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
        .merge(journal::router())
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
        let message = match &self {
            WebError::Validation(message) => Some(*message),
            _ => None,
        };
        let mut response =
            (status, axum::Json(serde_json::json!({ "error": code }))).into_response();
        if let Some(message) = message {
            response.extensions_mut().insert(ErrorMessage(message));
        }
        return response;
    }
}

#[derive(Clone, Copy)]
struct ErrorMessage(&'static str);

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage {
    heading: &'static str,
    message: String,
    back: Option<String>,
}

pub async fn html_errors(request: Request, next: Next) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let wants_html = request
        .headers()
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/html"));
    let is_htmx = is_htmx(request.headers());
    let back = same_origin_referer(request.headers());
    let response = next.run(request).await;
    let status = response.status();
    if is_api || is_htmx || !wants_html || !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    let replaceable = match response.headers().get(header::CONTENT_TYPE) {
        None => true,
        Some(value) => value.to_str().is_ok_and(|raw| raw.starts_with("application/json")),
    };
    if !replaceable {
        return response;
    }
    let detail = response.extensions().get::<ErrorMessage>().map(|message| message.0);
    let (heading, fallback) = match status {
        StatusCode::NOT_FOUND => ("Not found", "This page doesn't exist or isn't yours to see."),
        StatusCode::UNPROCESSABLE_ENTITY => ("That didn't work", "The request wasn't valid."),
        StatusCode::CONFLICT => ("Conflict", "That changed in the meantime. Reload and try again."),
        StatusCode::PAYLOAD_TOO_LARGE => ("Too large", "The upload is over the size limit."),
        StatusCode::BAD_REQUEST => ("Bad request", "The request couldn't be read."),
        _ => ("Something went wrong", "Try again in a moment."),
    };
    let message = match detail {
        Some(raw) => capitalize(raw),
        None => fallback.to_string(),
    };
    let page = ErrorPage {
        heading,
        message,
        back,
    };
    return match page.render() {
        Ok(body) => (
            status,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            body,
        )
            .into_response(),
        Err(_) => response,
    };
}

fn same_origin_referer(headers: &HeaderMap) -> Option<String> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let referer = headers.get(header::REFERER)?.to_str().ok()?;
    let rest = referer
        .strip_prefix("http://")
        .or_else(|| referer.strip_prefix("https://"))?;
    let path = rest.strip_prefix(host)?;
    if !path.starts_with('/') {
        return None;
    }
    return Some(path.to_string());
}

fn capitalize(raw: &str) -> String {
    let mut chars = raw.chars();
    return match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>() + ".",
        None => String::new(),
    };
}

pub type WebResult = Result<Response, WebError>;

pub async fn current_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<session::SessionUser>, WebError> {
    let Some(value) = headers.get(header::COOKIE) else {
        return Ok(None);
    };
    let Some(token) = token_from_header_value(value) else {
        return Ok(None);
    };
    return Ok(session::find_user_by_token(&state.pool, &token).await?);
}

#[derive(Debug, serde::Deserialize)]
pub struct ActionQuery {
    pub date: Option<String>,
    pub back: Option<String>,
    pub wd: Option<String>,
    pub who: Option<String>,
}

impl ActionQuery {
    pub fn redirect_target(&self, day: chrono::NaiveDate) -> String {
        let back = self.back.as_deref();
        if back == Some("week") || back == Some("month") {
            let week = self
                .wd
                .as_deref()
                .and_then(|raw| chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
            let who = self.who.as_deref().filter(|raw| {
                *raw == "me" || *raw == "all" || (!raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()))
            });
            if let (Some(week), Some(who)) = (week, who) {
                return format!("/{}?date={week}&who={who}", back.unwrap_or("week"));
            }
        }
        return format!("/day?date={day}");
    }
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
