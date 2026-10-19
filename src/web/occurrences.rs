use std::path::Path as StdPath;

use axum::Router;
use axum::extract::Form;
use axum::extract::Multipart;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::response::Redirect;
use axum::response::Response;
use axum::routing::post;
use axum::routing::put;
use chrono::NaiveDate;
use serde::Deserialize;
use sha2::Digest;
use sha2::Sha256;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::store;

use super::ActionQuery;
use super::WebError;
use super::WebResult;
use super::day::DayQuery;
use super::day::build_routines_section;
use super::hex_encode;
use super::html;
use super::is_htmx;
use super::parse_date_param;
use super::week::pin_fragment;

const MAX_PHOTO_BYTES: usize = 10 * 1024 * 1024;
const MAX_NOTES_CHARS: usize = 4000;

#[derive(Debug, Deserialize)]
struct TickPayload {
    evidence_note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CompletionPayload {
    evidence_note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PinJsonPayload {
    summary: Option<String>,
    notes: Option<String>,
    link_url: Option<String>,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/occurrences/{id}/tick", post(tick_ssr).delete(untick_ssr))
        .route("/occurrences/{id}/untick", post(untick_ssr))
        .route("/occurrences/{id}/pin", post(pin_ssr).get(pin_fragment))
        .route(
            "/api/v1/occurrences/{id}/completion",
            put(api_complete).delete(api_uncomplete),
        )
        .route("/api/v1/occurrences/{id}/pin", post(api_pin));
}

async fn tick_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
    Query(query): Query<ActionQuery>,
    headers: HeaderMap,
    Form(payload): Form<TickPayload>,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    let day = parse_date_param(query.date.as_deref(), today)?;
    ensure_today(day, today)?;
    let routine = load_active_routine(&state, auth.id, routine_id).await?;
    store::tick(&state.pool, &routine, day, payload.evidence_note.as_deref()).await?;
    return finish_ssr(&state, &auth, day, today, &headers, &query).await;
}

async fn untick_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
    Query(query): Query<ActionQuery>,
    headers: HeaderMap,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    let day = parse_date_param(query.date.as_deref(), today)?;
    ensure_today(day, today)?;
    load_active_routine(&state, auth.id, routine_id).await?;
    store::untick(&state.pool, routine_id, day).await?;
    return finish_ssr(&state, &auth, day, today, &headers, &query).await;
}

async fn pin_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(occurrence_id): Path<i64>,
    Query(query): Query<DayQuery>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    let day = parse_date_param(query.date.as_deref(), today)?;
    let mut summary: Option<String> = None;
    let mut notes: Option<String> = None;
    let mut link_url: Option<String> = None;
    let mut photos: Vec<(String, Vec<u8>)> = Vec::new();
    while let Some(field) = multipart.next_field().await? {
        match field.name() {
            Some("summary") => {
                summary = Some(field.text().await?);
            }
            Some("notes") => {
                notes = Some(field.text().await?);
            }
            Some("link_url") => {
                link_url = Some(field.text().await?);
            }
            Some("photos") => {
                let Some(file_name) = field.file_name().map(str::to_string) else {
                    continue;
                };
                let bytes = field.bytes().await?;
                if bytes.len() > MAX_PHOTO_BYTES {
                    return Err(WebError::Validation("photo exceeds 10 MiB"));
                }
                let ext = photo_extension(&file_name)?;
                photos.push((ext, bytes.to_vec()));
            }
            _ => continue,
        }
    }
    let summary = require_summary(summary.as_deref())?;
    let notes = normalize_notes(notes.as_deref())?;
    let link_url = validate_link(link_url.as_deref())?;
    let completion_id = load_completed_occurrence(&state, auth.id, occurrence_id).await?;
    let pin_id = store::upsert_pin(&state.pool, completion_id, &summary, notes.as_deref()).await?;
    if let Some(url) = link_url {
        store::add_link_attachment(&state.pool, pin_id, &url).await?;
    }
    for (ext, bytes) in photos {
        let digest = Sha256::digest(&bytes);
        let file_name = format!("{}.{ext}", hex_encode(&digest));
        tokio::fs::create_dir_all(store::UPLOADS_DIR).await?;
        tokio::fs::write(StdPath::new(store::UPLOADS_DIR).join(&file_name), &bytes).await?;
        store::add_photo_attachment(&state.pool, pin_id, &file_name).await?;
    }
    if is_htmx(&headers) {
        let section =
            build_routines_section(&state.pool, auth.id, &auth.timezone, day, today).await?;
        return Ok(html(&section)?);
    }
    return Ok(Redirect::to(&format!("/day?date={day}")).into_response());
}

async fn api_complete(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
    Query(query): Query<DayQuery>,
    axum::Json(payload): axum::Json<CompletionPayload>,
) -> Result<Response, WebError> {
    let today = store::user_today(&auth.timezone);
    let day = parse_date_param(query.date.as_deref(), today)?;
    ensure_today(day, today)?;
    let routine = load_active_routine(&state, auth.id, routine_id).await?;
    let occurrence_id =
        store::tick(&state.pool, &routine, day, payload.evidence_note.as_deref()).await?;
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({
            "ok": true,
            "occurrence_id": occurrence_id,
            "date": day.to_string(),
        })),
    )
        .into_response());
}

async fn api_uncomplete(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
    Query(query): Query<DayQuery>,
) -> Result<Response, WebError> {
    let today = store::user_today(&auth.timezone);
    let day = parse_date_param(query.date.as_deref(), today)?;
    ensure_today(day, today)?;
    load_active_routine(&state, auth.id, routine_id).await?;
    store::untick(&state.pool, routine_id, day).await?;
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true })),
    )
        .into_response());
}

async fn api_pin(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(occurrence_id): Path<i64>,
    axum::Json(payload): axum::Json<PinJsonPayload>,
) -> Result<Response, WebError> {
    let summary = require_summary(payload.summary.as_deref())?;
    let notes = normalize_notes(payload.notes.as_deref())?;
    let link_url = validate_link(payload.link_url.as_deref())?;
    let completion_id = load_completed_occurrence(&state, auth.id, occurrence_id).await?;
    let pin_id = store::upsert_pin(&state.pool, completion_id, &summary, notes.as_deref()).await?;
    if let Some(url) = link_url {
        store::add_link_attachment(&state.pool, pin_id, &url).await?;
    }
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true, "pin_id": pin_id })),
    )
        .into_response());
}

async fn finish_ssr(
    state: &AppState,
    auth: &crate::auth::session::SessionUser,
    day: NaiveDate,
    today: NaiveDate,
    headers: &HeaderMap,
    query: &ActionQuery,
) -> WebResult {
    if is_htmx(headers) {
        let section =
            build_routines_section(&state.pool, auth.id, &auth.timezone, day, today).await?;
        return Ok(html(&section)?);
    }
    return Ok(Redirect::to(&query.redirect_target(day)).into_response());
}

fn ensure_today(day: NaiveDate, today: NaiveDate) -> Result<(), WebError> {
    if day != today {
        return Err(WebError::Validation("only today's routines can be changed"));
    }
    return Ok(());
}

async fn load_active_routine(
    state: &AppState,
    user_id: i64,
    routine_id: i64,
) -> Result<store::RoutineRow, WebError> {
    let routine = store::routine_for_user(&state.pool, user_id, routine_id)
        .await?
        .ok_or(WebError::NotFound)?;
    if routine.archived_at.is_some() {
        return Err(WebError::NotFound);
    }
    return Ok(routine);
}

async fn load_completed_occurrence(
    state: &AppState,
    user_id: i64,
    occurrence_id: i64,
) -> Result<i64, WebError> {
    let owned = store::occurrence_owned(&state.pool, user_id, occurrence_id).await?;
    if !owned {
        return Err(WebError::NotFound);
    }
    return store::completion_id_for_occurrence(&state.pool, occurrence_id)
        .await?
        .ok_or(WebError::Conflict);
}

fn require_summary(raw: Option<&str>) -> Result<String, WebError> {
    let summary = raw.unwrap_or("").trim();
    if summary.is_empty() {
        return Err(WebError::Validation("summary is required"));
    }
    return Ok(summary.to_string());
}

fn normalize_notes(raw: Option<&str>) -> Result<Option<String>, WebError> {
    let trimmed = raw.unwrap_or("").trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_NOTES_CHARS {
        return Err(WebError::Validation(
            "notes are too long (max 4000 characters)",
        ));
    }
    return Ok(Some(trimmed.to_string()));
}

fn validate_link(raw: Option<&str>) -> Result<Option<String>, WebError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(WebError::Validation(
            "link must start with http:// or https://",
        ));
    }
    return Ok(Some(trimmed.to_string()));
}

fn photo_extension(file_name: &str) -> Result<String, WebError> {
    let ext = StdPath::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "jpg" {
        return Ok("jpeg".to_string());
    }
    if ext != "png" && ext != "jpeg" && ext != "webp" {
        return Err(WebError::Validation("photo must be png, jpeg or webp"));
    }
    return Ok(ext);
}
