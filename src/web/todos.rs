use axum::Router;
use axum::extract::Form;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::response::Redirect;
use axum::response::Response;
use axum::routing::delete;
use axum::routing::post;
use chrono::NaiveDate;
use chrono::NaiveDateTime;
use chrono::TimeZone;
use chrono::Utc;
use serde::Deserialize;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::store;

use super::WebError;
use super::WebResult;
use super::day::DayQuery;
use super::day::TodosSection;
use super::day::build_todos;
use super::html;
use super::is_htmx;
use super::parse_date_param;

#[derive(Debug, Deserialize)]
struct TodoForm {
    title: Option<String>,
    notes: Option<String>,
    deadline: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TodoJsonPayload {
    title: Option<String>,
    notes: Option<String>,
    deadline: Option<String>,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/todos", post(create_ssr))
        .route("/todos/{id}", delete(delete_ssr))
        .route("/todos/{id}/toggle", post(toggle_ssr))
        .route("/api/v1/todos", post(api_create))
        .route("/api/v1/todos/{id}", delete(api_delete))
        .route("/api/v1/todos/{id}/toggle", post(api_toggle));
}

async fn create_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Query(query): Query<DayQuery>,
    headers: HeaderMap,
    Form(payload): Form<TodoForm>,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    let day = parse_date_param(query.date.as_deref(), today)?;
    let deadline = parse_deadline(payload.deadline.as_deref(), &auth.timezone)?;
    let title = validate_title(payload.title.as_deref())?;
    store::create_todo(
        &state.pool,
        auth.id,
        &title,
        payload.notes.as_deref().map(str::trim),
        deadline.as_deref(),
    )
    .await?;
    return finish_ssr(&state, &auth, day, &headers).await;
}

async fn delete_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(todo_id): Path<i64>,
    Query(query): Query<DayQuery>,
    headers: HeaderMap,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    let day = parse_date_param(query.date.as_deref(), today)?;
    let deleted = store::delete_todo(&state.pool, auth.id, todo_id).await?;
    if !deleted {
        return Err(WebError::NotFound);
    }
    return finish_ssr(&state, &auth, day, &headers).await;
}

async fn toggle_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(todo_id): Path<i64>,
    Query(query): Query<DayQuery>,
    headers: HeaderMap,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    let day = parse_date_param(query.date.as_deref(), today)?;
    let toggled = store::toggle_todo(&state.pool, auth.id, todo_id).await?;
    if !toggled {
        return Err(WebError::NotFound);
    }
    return finish_ssr(&state, &auth, day, &headers).await;
}

async fn api_create(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    axum::Json(payload): axum::Json<TodoJsonPayload>,
) -> Result<Response, WebError> {
    let deadline = parse_deadline(payload.deadline.as_deref(), &auth.timezone)?;
    let title = validate_title(payload.title.as_deref())?;
    let id = store::create_todo(
        &state.pool,
        auth.id,
        &title,
        payload.notes.as_deref().map(str::trim),
        deadline.as_deref(),
    )
    .await?;
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "id": id, "title": title, "deadline": deadline })),
    )
        .into_response());
}

async fn api_delete(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(todo_id): Path<i64>,
) -> Result<Response, WebError> {
    let deleted = store::delete_todo(&state.pool, auth.id, todo_id).await?;
    if !deleted {
        return Err(WebError::NotFound);
    }
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true })),
    )
        .into_response());
}

async fn api_toggle(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(todo_id): Path<i64>,
) -> Result<Response, WebError> {
    let toggled = store::toggle_todo(&state.pool, auth.id, todo_id).await?;
    if !toggled {
        return Err(WebError::NotFound);
    }
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true })),
    )
        .into_response());
}

async fn finish_ssr(
    state: &AppState,
    auth: &crate::auth::session::SessionUser,
    day: NaiveDate,
    headers: &HeaderMap,
) -> WebResult {
    if is_htmx(headers) {
        let todos = build_todos(&state.pool, auth.id).await?;
        return Ok(html(&TodosSection { date: day, todos })?);
    }
    return Ok(Redirect::to(&format!("/day?date={day}")).into_response());
}

fn validate_title(raw: Option<&str>) -> Result<String, WebError> {
    let title = raw.unwrap_or("").trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(WebError::Validation("title must be 1-200 characters"));
    }
    return Ok(title.to_string());
}

fn parse_deadline(raw: Option<&str>, timezone: &str) -> Result<Option<String>, WebError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let naive = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M")
        .map_err(|_| WebError::Validation("deadline must be YYYY-MM-DDTHH:MM"))?;
    let tz = store::user_timezone(timezone);
    let local = match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(value) => value,
        chrono::LocalResult::Ambiguous(value, _) => value,
        chrono::LocalResult::None => {
            return Err(WebError::Validation(
                "deadline does not exist in user timezone",
            ));
        }
    };
    return Ok(Some(local.with_timezone(&Utc).to_rfc3339()));
}
