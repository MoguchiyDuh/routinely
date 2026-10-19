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
use axum::routing::get;
use axum::routing::post;
use chrono::NaiveDate;
use chrono::NaiveDateTime;
use chrono::NaiveTime;
use chrono::TimeZone;
use chrono::Utc;
use serde::Deserialize;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::store;

use super::ActionQuery;
use super::WebError;
use super::WebResult;
use super::day::DayQuery;
use super::day::build_todos_section;
use super::html;
use super::is_htmx;
use super::parse_date_param;

#[derive(Debug, Deserialize)]
struct TodoForm {
    title: Option<String>,
    notes: Option<String>,
    deadline: Option<String>,
    kind: Option<String>,
    date: Option<String>,
    time: Option<String>,
    reminders_json: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TodoJsonPayload {
    title: Option<String>,
    notes: Option<String>,
    deadline: Option<String>,
    kind: Option<String>,
    date: Option<String>,
    time: Option<String>,
    reminders: Option<Vec<store::ReminderSpec>>,
}

struct Due {
    deadline: Option<String>,
    all_day: bool,
    kind: &'static str,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/todos", get(region_ssr).post(create_ssr))
        .route("/todos/{id}", post(update_ssr).delete(delete_ssr))
        .route("/todos/{id}/delete", post(delete_ssr))
        .route("/todos/{id}/toggle", post(toggle_ssr))
        .route("/api/v1/todos", post(api_create))
        .route("/api/v1/todos/{id}", delete(api_delete).put(api_update))
        .route("/api/v1/todos/{id}/toggle", post(api_toggle));
}

async fn create_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Query(query): Query<DayQuery>,
    headers: HeaderMap,
    Form(payload): Form<TodoForm>,
) -> WebResult {
    let today = store::user_today(&auth.timezone, auth.day_start);
    let day = parse_date_param(query.date.as_deref(), today)?;
    let due = resolve_due(
        payload.kind.as_deref(),
        payload.deadline.as_deref(),
        payload.date.as_deref(),
        payload.time.as_deref(),
        Some(day),
        &auth.timezone,
    )?;
    let title = validate_title(payload.title.as_deref())?;
    let reminders = match non_empty(payload.reminders_json.as_deref()) {
        Some(raw) => {
            let list: Vec<store::ReminderSpec> =
                serde_json::from_str(raw).map_err(|_| WebError::Validation("invalid reminders"))?;
            if due.deadline.is_none() && !list.is_empty() {
                return Err(WebError::Validation("todo has no deadline"));
            }
            store::normalize_reminders(&list, !due.all_day)?;
            Some(list)
        }
        None => None,
    };
    let id = store::create_todo(
        &state.pool,
        auth.id,
        &title,
        payload.notes.as_deref().map(str::trim),
        due.deadline.as_deref(),
        due.kind,
        due.all_day,
    )
    .await?;
    match reminders {
        Some(list) if !list.is_empty() => {
            store::replace_reminders(&state.pool, auth.id, "todo", id, &list).await?;
        }
        Some(_) => {}
        None if due.deadline.is_some() => {
            store::apply_default_reminders(&state.pool, auth.id, "todo", id, !due.all_day).await?;
        }
        None => {}
    }
    return finish_ssr(&state, &auth, day, today, &headers).await;
}

async fn region_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Query(query): Query<DayQuery>,
    headers: HeaderMap,
) -> WebResult {
    let today = store::user_today(&auth.timezone, auth.day_start);
    let day = parse_date_param(query.date.as_deref(), today)?;
    if !is_htmx(&headers) {
        let target = match query.edit {
            Some(id) => format!("/day?date={day}&edit={id}"),
            None => format!("/day?date={day}"),
        };
        return Ok(Redirect::to(&target).into_response());
    }
    let section =
        build_todos_section(&state.pool, auth.id, &auth.timezone, day, today, query.edit).await?;
    return Ok(html(&section)?);
}

async fn update_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(todo_id): Path<i64>,
    Query(query): Query<DayQuery>,
    headers: HeaderMap,
    Form(payload): Form<TodoForm>,
) -> WebResult {
    let today = store::user_today(&auth.timezone, auth.day_start);
    let day = parse_date_param(query.date.as_deref(), today)?;
    let reminders = match non_empty(payload.reminders_json.as_deref()) {
        Some(raw) => Some(
            serde_json::from_str::<Vec<store::ReminderSpec>>(raw)
                .map_err(|_| WebError::Validation("invalid reminders"))?,
        ),
        None => None,
    };
    apply_update(
        &state,
        &auth,
        todo_id,
        Edit {
            title: payload.title.as_deref(),
            notes: payload.notes.as_deref(),
            deadline: payload.deadline.as_deref(),
            date: payload.date.as_deref(),
            time: payload.time.as_deref(),
            reminders: reminders.as_deref(),
        },
        Some(day),
    )
    .await?;
    return finish_ssr(&state, &auth, day, today, &headers).await;
}

struct Edit<'a> {
    title: Option<&'a str>,
    notes: Option<&'a str>,
    deadline: Option<&'a str>,
    date: Option<&'a str>,
    time: Option<&'a str>,
    reminders: Option<&'a [store::ReminderSpec]>,
}

async fn apply_update(
    state: &AppState,
    auth: &crate::auth::session::SessionUser,
    todo_id: i64,
    edit: Edit<'_>,
    default_day: Option<NaiveDate>,
) -> Result<Due, WebError> {
    let Some(existing) = store::todo_for_user(&state.pool, auth.id, todo_id).await? else {
        return Err(WebError::NotFound);
    };
    if existing.completed_at.is_some() {
        return Err(WebError::Conflict);
    }
    let due = resolve_due(
        Some(existing.kind.as_str()),
        edit.deadline,
        edit.date,
        edit.time,
        default_day,
        &auth.timezone,
    )?;
    let title = validate_title(edit.title)?;
    if let Some(list) = edit.reminders
        && due.deadline.is_some()
    {
        store::normalize_reminders(list, !due.all_day)?;
    }
    let updated = store::update_todo(
        &state.pool,
        auth.id,
        todo_id,
        &title,
        non_empty(edit.notes),
        due.deadline.as_deref(),
        due.all_day,
    )
    .await?;
    if !updated {
        return Err(WebError::Conflict);
    }
    match (due.deadline.is_some(), edit.reminders) {
        (false, _) => store::clear_reminders(&state.pool, auth.id, "todo", todo_id).await?,
        (true, Some(list)) => {
            store::replace_reminders(&state.pool, auth.id, "todo", todo_id, list).await?;
        }
        (true, None) if existing.all_day != due.all_day || existing.deadline.is_none() => {
            store::clear_reminders(&state.pool, auth.id, "todo", todo_id).await?;
            store::apply_default_reminders(&state.pool, auth.id, "todo", todo_id, !due.all_day)
                .await?;
        }
        (true, None) => {}
    }
    return Ok(due);
}

async fn api_update(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(todo_id): Path<i64>,
    axum::Json(payload): axum::Json<TodoJsonPayload>,
) -> Result<Response, WebError> {
    let title = payload.title.as_deref().map(str::trim).map(str::to_string);
    let due = apply_update(
        &state,
        &auth,
        todo_id,
        Edit {
            title: title.as_deref(),
            notes: payload.notes.as_deref(),
            deadline: payload.deadline.as_deref(),
            date: payload.date.as_deref(),
            time: payload.time.as_deref(),
            reminders: payload.reminders.as_deref(),
        },
        None,
    )
    .await?;
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(
            serde_json::json!({ "id": todo_id, "title": title, "deadline": due.deadline, "kind": due.kind, "all_day": due.all_day }),
        ),
    )
        .into_response());
}

async fn delete_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(todo_id): Path<i64>,
    Query(query): Query<DayQuery>,
    headers: HeaderMap,
) -> WebResult {
    let today = store::user_today(&auth.timezone, auth.day_start);
    let day = parse_date_param(query.date.as_deref(), today)?;
    let deleted = store::delete_todo(&state.pool, auth.id, todo_id).await?;
    if !deleted {
        return Err(WebError::NotFound);
    }
    return finish_ssr(&state, &auth, day, today, &headers).await;
}

async fn toggle_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(todo_id): Path<i64>,
    Query(query): Query<ActionQuery>,
    headers: HeaderMap,
) -> WebResult {
    let today = store::user_today(&auth.timezone, auth.day_start);
    let day = parse_date_param(query.date.as_deref(), today)?;
    let toggled = store::toggle_todo(&state.pool, auth.id, todo_id).await?;
    if !toggled {
        return Err(WebError::NotFound);
    }
    if !is_htmx(&headers) {
        return Ok(Redirect::to(&query.redirect_target(day)).into_response());
    }
    return finish_ssr(&state, &auth, day, today, &headers).await;
}

async fn api_create(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    axum::Json(payload): axum::Json<TodoJsonPayload>,
) -> Result<Response, WebError> {
    let due = resolve_due(
        payload.kind.as_deref(),
        payload.deadline.as_deref(),
        payload.date.as_deref(),
        payload.time.as_deref(),
        None,
        &auth.timezone,
    )?;
    let title = validate_title(payload.title.as_deref())?;
    if let Some(reminders) = &payload.reminders {
        if due.deadline.is_none() && !reminders.is_empty() {
            return Err(WebError::Validation("todo has no deadline"));
        }
        store::normalize_reminders(reminders, !due.all_day)?;
    }
    let id = store::create_todo(
        &state.pool,
        auth.id,
        &title,
        payload.notes.as_deref().map(str::trim),
        due.deadline.as_deref(),
        due.kind,
        due.all_day,
    )
    .await?;
    match payload.reminders.as_deref() {
        Some([]) => {}
        Some(reminders) => {
            store::replace_reminders(&state.pool, auth.id, "todo", id, reminders).await?;
        }
        None if due.deadline.is_some() => {
            store::apply_default_reminders(&state.pool, auth.id, "todo", id, !due.all_day).await?;
        }
        None => {}
    }
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(
            serde_json::json!({ "id": id, "title": title, "deadline": due.deadline, "kind": due.kind, "all_day": due.all_day }),
        ),
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
    today: NaiveDate,
    headers: &HeaderMap,
) -> WebResult {
    if is_htmx(headers) {
        let section =
            build_todos_section(&state.pool, auth.id, &auth.timezone, day, today, None).await?;
        return Ok(html(&section)?);
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

fn non_empty(raw: Option<&str>) -> Option<&str> {
    return raw.map(str::trim).filter(|value| !value.is_empty());
}

fn resolve_due(
    kind_raw: Option<&str>,
    deadline_raw: Option<&str>,
    date_raw: Option<&str>,
    time_raw: Option<&str>,
    default_day: Option<NaiveDate>,
    timezone: &str,
) -> Result<Due, WebError> {
    let kind = match non_empty(kind_raw) {
        None | Some("todo") => "todo",
        Some("deadline") => "deadline",
        Some(_) => return Err(WebError::Validation("kind must be todo or deadline")),
    };
    let time = match non_empty(time_raw) {
        Some(raw) => Some(
            NaiveTime::parse_from_str(raw, "%H:%M")
                .map_err(|_| WebError::Validation("time must be HH:MM"))?,
        ),
        None => None,
    };
    let date = match non_empty(date_raw) {
        Some(raw) => Some(
            NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .map_err(|_| WebError::Validation("date must be YYYY-MM-DD"))?,
        ),
        None => None,
    };
    let date = match (date, time) {
        (None, Some(_)) => default_day,
        (value, _) => value,
    };
    let (deadline, all_day) = match date {
        Some(day) => {
            let end_of_day = NaiveTime::from_hms_opt(23, 59, 59).unwrap_or(NaiveTime::MIN);
            let naive = day.and_time(time.unwrap_or(end_of_day));
            (Some(local_to_utc(naive, timezone)?), time.is_none())
        }
        None => (parse_deadline(deadline_raw, timezone)?, false),
    };
    if kind == "deadline" && deadline.is_none() {
        return Err(WebError::Validation("a deadline needs a date"));
    }
    return Ok(Due {
        deadline,
        all_day,
        kind,
    });
}

fn local_to_utc(naive: NaiveDateTime, timezone: &str) -> Result<String, WebError> {
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
    return Ok(local.with_timezone(&Utc).to_rfc3339());
}

fn parse_deadline(raw: Option<&str>, timezone: &str) -> Result<Option<String>, WebError> {
    let Some(trimmed) = non_empty(raw) else {
        return Ok(None);
    };
    let naive = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M")
        .map_err(|_| WebError::Validation("deadline must be YYYY-MM-DDTHH:MM"))?;
    return Ok(Some(local_to_utc(naive, timezone)?));
}
