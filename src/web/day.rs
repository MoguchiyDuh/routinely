use askama::Template;
use axum::Router;
use axum::extract::Query;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::Redirect;
use axum::routing::get;
use chrono::NaiveDate;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use sqlx::SqlitePool;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::store;

use super::WebError;
use super::WebResult;
use super::html;
use super::parse_date_param;
use super::shell::Shell;

#[derive(Template)]
#[template(path = "day.html")]
pub struct DayPage {
    pub shell: Shell,
    date: NaiveDate,
    prev_date: NaiveDate,
    next_date: NaiveDate,
    is_today: bool,
    rows: Vec<RowVm>,
    todos: Vec<TodoVm>,
    body: Option<String>,
}

#[derive(Template)]
#[template(path = "_routine.html")]
pub(crate) struct RoutinesSection {
    pub date: NaiveDate,
    pub rows: Vec<RowVm>,
}

#[derive(Template)]
#[template(path = "_todo.html")]
pub(crate) struct TodosSection {
    pub date: NaiveDate,
    pub todos: Vec<TodoVm>,
}

#[derive(Template)]
#[template(path = "_note.html")]
pub(crate) struct NoteSection {
    pub date: NaiveDate,
    pub body: Option<String>,
}

pub(crate) struct RowVm {
    pub id: i64,
    pub title: String,
    pub status: &'static str,
    pub occurrence_id: Option<i64>,
    pub evidence_note: Option<String>,
    pub period_done: Option<u32>,
    pub period_required: Option<u32>,
    pub pin: Option<PinVm>,
    pub time_label: Option<String>,
}

pub(crate) struct PinVm {
    pub summary: String,
    pub links: Vec<String>,
    pub photos: Vec<String>,
}

pub(crate) struct TodoVm {
    pub id: i64,
    pub title: String,
    pub notes: Option<String>,
    pub deadline: Option<String>,
    pub done: bool,
    pub overdue: bool,
}

#[derive(Debug, Deserialize)]
pub struct DayQuery {
    pub date: Option<String>,
}

#[derive(Serialize)]
struct DayJson {
    date: NaiveDate,
    routines: Vec<RoutineJson>,
    todos: Vec<TodoJson>,
    note: Option<String>,
}

#[derive(Serialize)]
struct RoutineJson {
    id: i64,
    title: String,
    kind: String,
    status: &'static str,
    completed: bool,
    occurrence_id: Option<i64>,
    evidence_note: Option<String>,
    period_done: Option<u32>,
    period_required: Option<u32>,
}

#[derive(Serialize)]
struct TodoJson {
    id: i64,
    title: String,
    notes: Option<String>,
    deadline: Option<String>,
    completed_at: Option<String>,
    overdue: bool,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/", get(root))
        .route("/day", get(day_view))
        .route("/api/v1/day", get(api_day));
}

async fn root(State(state): State<AppState>, headers: axum::http::HeaderMap) -> WebResult {
    let user = super::current_user(&state, &headers)
        .await
        .map_err(WebError::from)?;
    if user.is_some() {
        return Ok(Redirect::to("/day").into_response());
    }
    return Ok(Redirect::to("/login").into_response());
}

async fn day_view(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Query(query): Query<DayQuery>,
    headers: axum::http::HeaderMap,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    let date = parse_date_param(query.date.as_deref(), today)?;
    let mut conn = state.pool.acquire().await?;
    store::materialize(&mut conn, auth.id, today).await?;
    drop(conn);
    let shell = Shell::load(&state, &auth, "today", &headers).await?;
    let rows = build_rows(&state.pool, auth.id, date, today).await?;
    let todos = build_todos(&state.pool, auth.id).await?;
    let body = store::get_note(&state.pool, auth.id, date).await?;
    let page = DayPage {
        shell,
        date,
        prev_date: prev_day(date),
        next_date: next_day(date),
        is_today: date == today,
        rows,
        todos,
        body,
    };
    return Ok(html(&page)?);
}

async fn api_day(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Query(query): Query<DayQuery>,
) -> Result<axum::Json<DayJson>, WebError> {
    let today = store::user_today(&auth.timezone);
    let date = parse_date_param(query.date.as_deref(), today)?;
    let mut conn = state.pool.acquire().await?;
    store::materialize(&mut conn, auth.id, today).await?;
    drop(conn);
    let infos = store::day_routines(&state.pool, auth.id, date, today).await?;
    let mut routines = Vec::new();
    for info in infos {
        routines.push(RoutineJson {
            id: info.routine.id,
            title: info.routine.title.clone(),
            kind: info.routine.kind.clone(),
            status: info.status,
            completed: info.completed,
            occurrence_id: info.occurrence_id,
            evidence_note: info.evidence_note,
            period_done: info.period_done,
            period_required: info.period_required,
        });
    }
    let todo_rows = store::list_todos(&state.pool, auth.id, &Utc::now().to_rfc3339()).await?;
    let todos = todo_rows
        .iter()
        .map(|t| TodoJson {
            id: t.id,
            title: t.title.clone(),
            notes: t.notes.clone(),
            deadline: t.deadline.clone(),
            completed_at: t.completed_at.clone(),
            overdue: is_overdue(t),
        })
        .collect();
    let note = store::get_note(&state.pool, auth.id, date).await?;
    return Ok(axum::Json(DayJson {
        date,
        routines,
        todos,
        note,
    }));
}

pub(crate) async fn build_rows(
    pool: &SqlitePool,
    user_id: i64,
    date: NaiveDate,
    today: NaiveDate,
) -> Result<Vec<RowVm>, WebError> {
    let infos = store::day_routines(pool, user_id, date, today).await?;
    let mut rows = Vec::new();
    for info in infos {
        let pin =
            if info.completed {
                match info.occurrence_id {
                    Some(occurrence_id) => store::pin_for_occurrence(pool, occurrence_id)
                        .await?
                        .map(|p| PinVm {
                            summary: p.summary,
                            links: p.links,
                            photos: p.photos,
                        }),
                    None => None,
                }
            } else {
                None
            };
        let time_label = build_time_label(&info.routine);
        rows.push(RowVm {
            id: info.routine.id,
            title: info.routine.title.clone(),
            status: info.status,
            occurrence_id: info.occurrence_id,
            evidence_note: info.evidence_note,
            period_done: info.period_done,
            period_required: info.period_required,
            pin,
            time_label,
        });
    }
    return Ok(rows);
}

pub(crate) async fn build_todos(pool: &SqlitePool, user_id: i64) -> Result<Vec<TodoVm>, WebError> {
    let now = Utc::now().to_rfc3339();
    let todo_rows = store::list_todos(pool, user_id, &now).await?;
    return Ok(todo_rows
        .iter()
        .map(|t| TodoVm {
            id: t.id,
            title: t.title.clone(),
            notes: t.notes.clone(),
            deadline: t.deadline.clone(),
            done: t.completed_at.is_some(),
            overdue: is_overdue(t),
        })
        .collect());
}

pub(crate) fn prev_day(date: NaiveDate) -> NaiveDate {
    return date.pred_opt().unwrap_or(date);
}

pub(crate) fn next_day(date: NaiveDate) -> NaiveDate {
    return date.succ_opt().unwrap_or(date);
}

fn is_overdue(todo: &store::TodoRow) -> bool {
    if todo.completed_at.is_some() {
        return false;
    }
    let Some(deadline) = &todo.deadline else {
        return false;
    };
    return deadline.as_str() < Utc::now().to_rfc3339().as_str();
}

fn build_time_label(routine: &store::RoutineRow) -> Option<String> {
    let start = routine.time_of_day.clone()?;
    return match &routine.time_end {
        Some(end) => Some(format!("{start} - {end}")),
        None => Some(start),
    };
}
