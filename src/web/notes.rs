use axum::Router;
use axum::extract::Form;
use axum::extract::Path;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::response::Redirect;
use axum::response::Response;
use axum::routing::post;
use axum::routing::put;
use chrono::NaiveDate;
use serde::Deserialize;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::store;

use super::WebError;
use super::WebResult;
use super::day::NoteSection;
use super::html;
use super::is_htmx;

#[derive(Debug, Deserialize)]
struct NoteForm {
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NoteJsonPayload {
    body: Option<String>,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/notes/{date}", post(save_ssr))
        .route("/api/v1/notes/{date}", put(api_save));
}

async fn save_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(date_raw): Path<String>,
    headers: HeaderMap,
    Form(payload): Form<NoteForm>,
) -> WebResult {
    let day = parse_date(&date_raw)?;
    let body = payload.body.unwrap_or_default();
    store::upsert_note(&state.pool, auth.id, day, &body).await?;
    if is_htmx(&headers) {
        let body = store::get_note(&state.pool, auth.id, day).await?;
        return Ok(html(&NoteSection {
            date: day,
            body,
            saved: true,
            readonly: false,
        })?);
    }
    return Ok(Redirect::to(&format!("/day?date={day}")).into_response());
}

async fn api_save(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(date_raw): Path<String>,
    axum::Json(payload): axum::Json<NoteJsonPayload>,
) -> Result<Response, WebError> {
    let day = parse_date(&date_raw)?;
    let body = payload.body.unwrap_or_default();
    store::upsert_note(&state.pool, auth.id, day, &body).await?;
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true, "date": day.to_string() })),
    )
        .into_response());
}

fn parse_date(raw: &str) -> Result<NaiveDate, WebError> {
    return NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map_err(|_| WebError::Validation("invalid date"));
}
