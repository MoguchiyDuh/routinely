use askama::Template;
use axum::Router;
use axum::extract::Query;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::Redirect;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use chrono::DateTime;
use chrono::NaiveDate;
use chrono::Timelike;
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
    weekday: String,
    date_label: String,
    prev_date: NaiveDate,
    next_date: NaiveDate,
    is_today: bool,
    onboarding: Option<OnboardingVm>,
    progress_done: u32,
    progress_total: u32,
    progress_percent: u32,
    has_routines: bool,
    rows: Vec<RowVm>,
    blocks: Vec<BlockVm>,
    markers: Vec<MarkerVm>,
    hours: Vec<String>,
    now_min: i64,
    overdue: Vec<TodoVm>,
    todos: Vec<TodoVm>,
    body: Option<String>,
    saved: bool,
}

#[derive(Template)]
#[template(path = "_routine.html")]
pub(crate) struct RoutinesSection {
    pub date: NaiveDate,
    pub is_today: bool,
    pub progress_done: u32,
    pub progress_total: u32,
    pub progress_percent: u32,
    pub has_routines: bool,
    pub rows: Vec<RowVm>,
    pub blocks: Vec<BlockVm>,
    pub markers: Vec<MarkerVm>,
    pub hours: Vec<String>,
    pub now_min: i64,
}

#[derive(Template)]
#[template(path = "_todo.html")]
pub(crate) struct TodosSection {
    pub date: NaiveDate,
    pub is_today: bool,
    pub overdue: Vec<TodoVm>,
    pub todos: Vec<TodoVm>,
    pub body: Option<String>,
    pub saved: bool,
}

#[derive(Template)]
#[template(path = "_note.html")]
pub(crate) struct NoteSection {
    pub date: NaiveDate,
    pub body: Option<String>,
    pub saved: bool,
}

pub(crate) struct OnboardingVm {
    pub steps: Vec<OnboardingStep>,
}

pub(crate) struct OnboardingStep {
    pub label: &'static str,
    pub done: bool,
    pub icon: &'static str,
    pub href: Option<&'static str>,
    pub action: &'static str,
}

pub(crate) struct RowVm {
    pub id: i64,
    pub title: String,
    pub status: &'static str,
    pub status_word: &'static str,
    pub pill_class: &'static str,
    pub pill_icon: &'static str,
    pub occurrence_id: Option<i64>,
    pub period_done: Option<u32>,
    pub period_required: Option<u32>,
    pub period_name: Option<&'static str>,
    pub pin: Option<PinVm>,
}

pub(crate) struct BlockVm {
    pub id: i64,
    pub title: String,
    pub status: &'static str,
    pub status_word: &'static str,
    pub pill_class: &'static str,
    pub pill_icon: &'static str,
    pub occurrence_id: Option<i64>,
    pub start_min: i64,
    pub len_min: i64,
    pub col: u32,
    pub cols: u32,
    pub time_label: String,
    pub pin: Option<PinVm>,
}

pub(crate) struct MarkerVm {
    pub title: String,
    pub time_label: String,
    pub start_min: i64,
    pub done: bool,
    pub overdue: bool,
}

pub(crate) struct PinVm {
    pub summary: String,
    pub thumb: Option<String>,
    pub links: Vec<String>,
}

pub(crate) struct TodoVm {
    pub id: i64,
    pub title: String,
    pub notes: Option<String>,
    pub deadline: Option<String>,
    pub done: bool,
    pub overdue: bool,
}

struct RoutineData {
    progress_done: u32,
    progress_total: u32,
    has_routines: bool,
    rows: Vec<RowVm>,
    blocks: Vec<BlockVm>,
}

struct PendingBlock {
    block: BlockVm,
    start: i64,
    end: i64,
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
        .route("/onboarding/dismiss", post(dismiss_ssr))
        .route("/api/v1/me/onboarding/dismiss", post(api_dismiss))
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
    let data = build_routine_data(&state.pool, auth.id, date, today).await?;
    let markers = build_markers(&state.pool, auth.id, &auth.timezone, date).await?;
    let (overdue, todos) = build_todo_lists(&state.pool, auth.id, &auth.timezone).await?;
    let body = store::get_note(&state.pool, auth.id, date).await?;
    let onboarding = build_onboarding(&state.pool, auth.id).await?;
    let progress_total = data.progress_total;
    let progress_percent = progress_percent(data.progress_done, progress_total);
    let now_min = local_now_minutes(&auth.timezone);
    let page = DayPage {
        shell,
        date,
        weekday: date.format("%A").to_string(),
        date_label: date.format("%d %B %Y").to_string(),
        prev_date: prev_day(date),
        next_date: next_day(date),
        is_today: date == today,
        onboarding,
        progress_done: data.progress_done,
        progress_total,
        progress_percent,
        has_routines: data.has_routines,
        rows: data.rows,
        blocks: data.blocks,
        markers,
        hours: hour_labels(),
        now_min,
        overdue,
        todos,
        body,
        saved: false,
    };
    return Ok(html(&page)?);
}

async fn dismiss_ssr(PageAuth(auth): PageAuth, State(state): State<AppState>) -> WebResult {
    store::dismiss_onboarding(&state.pool, auth.id).await?;
    return Ok(super::shell::redirect_with_flash(
        "/day",
        "Checklist dismissed",
        state.config.cookie_secure,
    ));
}

async fn api_dismiss(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
) -> Result<Response, WebError> {
    store::dismiss_onboarding(&state.pool, auth.id).await?;
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true })),
    )
        .into_response());
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
    let now = Utc::now().to_rfc3339();
    let todo_rows = store::list_todos(&state.pool, auth.id, &now).await?;
    let todos = todo_rows
        .iter()
        .map(|t| TodoJson {
            id: t.id,
            title: t.title.clone(),
            notes: t.notes.clone(),
            deadline: t.deadline.clone(),
            completed_at: t.completed_at.clone(),
            overdue: is_overdue(t, &now),
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

async fn build_routine_data(
    pool: &SqlitePool,
    user_id: i64,
    date: NaiveDate,
    today: NaiveDate,
) -> Result<RoutineData, WebError> {
    let infos = store::day_routines(pool, user_id, date, today).await?;
    let has_routines = store::has_any_routine(pool, user_id).await?;
    let mut progress_done = 0;
    let mut progress_total = 0;
    let mut rows = Vec::new();
    let mut pending: Vec<PendingBlock> = Vec::new();
    for info in infos {
        let is_count = info.routine.kind == "count";
        let quota_met = match (info.period_done, info.period_required) {
            (Some(done), Some(required)) => done >= required,
            _ => false,
        };
        if if is_count { quota_met } else { info.completed } {
            progress_done += 1;
        }
        progress_total += 1;
        let pin = load_pin(pool, &info).await?;
        let (pill_class, pill_icon, status_word) = status_meta(info.status);
        let start = if is_count {
            None
        } else {
            parse_time_minutes(info.routine.time_of_day.as_deref())
        };
        match start {
            Some(start_min) => {
                let len_min = block_len(
                    start_min,
                    parse_time_minutes(info.routine.time_end.as_deref()),
                );
                pending.push(PendingBlock {
                    block: BlockVm {
                        id: info.routine.id,
                        title: info.routine.title.clone(),
                        status: info.status,
                        status_word,
                        pill_class,
                        pill_icon,
                        occurrence_id: info.occurrence_id,
                        start_min,
                        len_min,
                        col: 1,
                        cols: 1,
                        time_label: format!(
                            "{}\u{2013}{}",
                            fmt_minutes(start_min),
                            fmt_minutes(start_min + len_min)
                        ),
                        pin,
                    },
                    start: start_min,
                    end: start_min + len_min,
                });
            }
            None => {
                let period_name = if is_count {
                    info.routine.period.map(period_name)
                } else {
                    None
                };
                rows.push(RowVm {
                    id: info.routine.id,
                    title: info.routine.title.clone(),
                    status: info.status,
                    status_word,
                    pill_class,
                    pill_icon,
                    occurrence_id: info.occurrence_id,
                    period_done: info.period_done,
                    period_required: info.period_required,
                    period_name,
                    pin,
                });
            }
        }
    }
    pending.sort_by_key(|item| (item.start, item.end));
    let spans: Vec<(i64, i64)> = pending.iter().map(|p| (p.start, p.end)).collect();
    let assigned = overlap_columns(&spans);
    let mut blocks = Vec::new();
    for (item, (col, cols)) in pending.into_iter().zip(assigned) {
        let mut block = item.block;
        block.col = col;
        block.cols = cols;
        blocks.push(block);
    }
    return Ok(RoutineData {
        progress_done,
        progress_total,
        has_routines,
        rows,
        blocks,
    });
}

pub fn overlap_columns(spans: &[(i64, i64)]) -> Vec<(u32, u32)> {
    let count = spans.len();
    let mut result: Vec<(u32, u32)> = vec![(1, 1); count];
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|&index| (spans[index].0, std::cmp::Reverse(spans[index].1)));
    let mut cursor = 0;
    while cursor < count {
        let first = order[cursor];
        let mut cluster = vec![first];
        let mut cluster_end = spans[first].1;
        let mut probe = cursor + 1;
        while probe < count && spans[order[probe]].0 < cluster_end {
            let index = order[probe];
            if spans[index].1 > cluster_end {
                cluster_end = spans[index].1;
            }
            cluster.push(index);
            probe += 1;
        }
        let mut column_ends: Vec<i64> = Vec::new();
        for &index in &cluster {
            let (start, end) = spans[index];
            let mut column = 0;
            while column < column_ends.len() && column_ends[column] > start {
                column += 1;
            }
            if column == column_ends.len() {
                column_ends.push(end);
            } else {
                column_ends[column] = end;
            }
            result[index].0 = (column + 1) as u32;
        }
        let total = column_ends.len() as u32;
        for &index in &cluster {
            result[index].1 = total;
        }
        cursor = probe;
    }
    return result;
}

pub(crate) async fn build_routines_section(
    pool: &SqlitePool,
    user_id: i64,
    timezone: &str,
    date: NaiveDate,
    today: NaiveDate,
) -> Result<RoutinesSection, WebError> {
    let data = build_routine_data(pool, user_id, date, today).await?;
    let markers = build_markers(pool, user_id, timezone, date).await?;
    let progress_percent = progress_percent(data.progress_done, data.progress_total);
    return Ok(RoutinesSection {
        date,
        is_today: date == today,
        progress_done: data.progress_done,
        progress_total: data.progress_total,
        progress_percent,
        has_routines: data.has_routines,
        rows: data.rows,
        blocks: data.blocks,
        markers,
        hours: hour_labels(),
        now_min: local_now_minutes(timezone),
    });
}

pub(crate) async fn build_todos_section(
    pool: &SqlitePool,
    user_id: i64,
    timezone: &str,
    date: NaiveDate,
    today: NaiveDate,
) -> Result<TodosSection, WebError> {
    let (overdue, todos) = build_todo_lists(pool, user_id, timezone).await?;
    let body = store::get_note(pool, user_id, date).await?;
    return Ok(TodosSection {
        date,
        is_today: date == today,
        overdue,
        todos,
        body,
        saved: false,
    });
}

async fn build_todo_lists(
    pool: &SqlitePool,
    user_id: i64,
    timezone: &str,
) -> Result<(Vec<TodoVm>, Vec<TodoVm>), WebError> {
    let now = Utc::now().to_rfc3339();
    let tz = store::user_timezone(timezone);
    let todo_rows = store::list_todos(pool, user_id, &now).await?;
    let mut overdue = Vec::new();
    let mut undated = Vec::new();
    for t in &todo_rows {
        let vm = TodoVm {
            id: t.id,
            title: t.title.clone(),
            notes: t.notes.clone(),
            deadline: t.deadline.as_deref().map(|raw| fmt_deadline(raw, &tz)),
            done: t.completed_at.is_some(),
            overdue: is_overdue(t, &now),
        };
        if vm.overdue {
            overdue.push(vm);
        } else if t.deadline.is_none() {
            undated.push(vm);
        }
    }
    return Ok((overdue, undated));
}

async fn build_markers(
    pool: &SqlitePool,
    user_id: i64,
    timezone: &str,
    date: NaiveDate,
) -> Result<Vec<MarkerVm>, WebError> {
    let now = Utc::now().to_rfc3339();
    let tz = store::user_timezone(timezone);
    let todo_rows = store::list_todos(pool, user_id, &now).await?;
    let mut markers = Vec::new();
    for t in &todo_rows {
        let Some(deadline) = t.deadline.as_deref() else {
            continue;
        };
        let Ok(parsed) = DateTime::parse_from_rfc3339(deadline) else {
            continue;
        };
        let local = parsed.with_timezone(&tz);
        if local.date_naive() != date {
            continue;
        }
        markers.push(MarkerVm {
            title: t.title.clone(),
            time_label: local.format("%H:%M").to_string(),
            start_min: i64::from(local.hour()) * 60 + i64::from(local.minute()),
            done: t.completed_at.is_some(),
            overdue: is_overdue(t, &now),
        });
    }
    markers.sort_by_key(|m| m.start_min);
    return Ok(markers);
}

async fn build_onboarding(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<Option<OnboardingVm>, WebError> {
    let flags = store::onboarding_flags(pool, user_id).await?;
    if flags.dismissed {
        return Ok(None);
    }
    if flags.has_routine && flags.has_completion && flags.has_pin && flags.avatar_set {
        return Ok(None);
    }
    let steps = vec![
        OnboardingStep {
            label: "Create your first routine",
            done: flags.has_routine,
            icon: step_icon(flags.has_routine),
            href: Some("/routines/new"),
            action: "Create routine",
        },
        OnboardingStep {
            label: "Complete a routine",
            done: flags.has_completion,
            icon: step_icon(flags.has_completion),
            href: None,
            action: "",
        },
        OnboardingStep {
            label: "Add proof to a completion",
            done: flags.has_pin,
            icon: step_icon(flags.has_pin),
            href: None,
            action: "",
        },
        OnboardingStep {
            label: "Set your avatar",
            done: flags.avatar_set,
            icon: step_icon(flags.avatar_set),
            href: Some("/settings"),
            action: "Open settings",
        },
    ];
    return Ok(Some(OnboardingVm { steps }));
}

async fn load_pin(
    pool: &SqlitePool,
    info: &store::DayRoutineInfo,
) -> Result<Option<PinVm>, WebError> {
    if !info.completed {
        return Ok(None);
    }
    let Some(occurrence_id) = info.occurrence_id else {
        return Ok(None);
    };
    let pin = store::pin_for_occurrence(pool, occurrence_id).await?;
    return Ok(pin.map(|p| PinVm {
        summary: p.summary,
        thumb: p.photos.first().map(|photo| format!("/uploads/{photo}")),
        links: p.links,
    }));
}

pub(crate) fn prev_day(date: NaiveDate) -> NaiveDate {
    return date.pred_opt().unwrap_or(date);
}

pub(crate) fn next_day(date: NaiveDate) -> NaiveDate {
    return date.succ_opt().unwrap_or(date);
}

fn is_overdue(todo: &store::TodoRow, now: &str) -> bool {
    if todo.completed_at.is_some() {
        return false;
    }
    let Some(deadline) = &todo.deadline else {
        return false;
    };
    return deadline.as_str() < now;
}

pub(crate) fn status_meta(status: &'static str) -> (&'static str, &'static str, &'static str) {
    return match status {
        "completed" => ("pill-done", "circle-check", "done"),
        "missed" => ("pill-missed", "circle-alert", "missed"),
        _ => ("pill-open", "circle-dashed", "open"),
    };
}

fn step_icon(done: bool) -> &'static str {
    if done {
        return "circle-check";
    }
    return "circle-dashed";
}

fn period_name(period: store::PeriodKind) -> &'static str {
    return match period {
        store::PeriodKind::Day => "day",
        store::PeriodKind::Week => "week",
        store::PeriodKind::Month => "month",
        store::PeriodKind::Year => "year",
    };
}

pub(crate) fn parse_time_minutes(raw: Option<&str>) -> Option<i64> {
    let raw = raw?;
    let (hours, minutes) = raw.trim().split_once(':')?;
    let hours: i64 = hours.trim().parse().ok()?;
    let minutes: i64 = minutes.trim().parse().ok()?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    return Some(hours * 60 + minutes);
}

pub(crate) fn block_len(start: i64, end: Option<i64>) -> i64 {
    return match end {
        Some(end) if end > start => end - start,
        _ => 30,
    };
}

fn fmt_minutes(minutes: i64) -> String {
    let normalized = minutes.rem_euclid(24 * 60);
    return format!("{:02}:{:02}", normalized / 60, normalized % 60);
}

fn fmt_deadline(raw: &str, tz: &chrono_tz::Tz) -> String {
    return match DateTime::parse_from_rfc3339(raw) {
        Ok(parsed) => parsed.with_timezone(tz).format("%d %b %H:%M").to_string(),
        Err(_) => raw.to_string(),
    };
}

fn progress_percent(done: u32, total: u32) -> u32 {
    if total == 0 {
        return 0;
    }
    return done * 100 / total;
}

pub(crate) fn hour_labels() -> Vec<String> {
    return (0..24).map(|hour| format!("{hour:02}:00")).collect();
}

pub(crate) fn local_now_minutes(timezone: &str) -> i64 {
    let now = Utc::now().with_timezone(&store::user_timezone(timezone));
    return i64::from(now.hour()) * 60 + i64::from(now.minute());
}
