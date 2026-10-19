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
use chrono::Datelike;
use chrono::NaiveDate;
use chrono::Timelike;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use sqlx::PgPool;

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
    now_label: String,
    now_at_end: bool,
    overdue: Vec<TodoVm>,
    todos: Vec<TodoVm>,
    deadlines: Vec<TodoVm>,
    body: Option<String>,
    saved: bool,
    readonly: bool,
    locked: bool,
    picker: super::week::PickerVm,
    who_q: String,
    who_param: String,
}

#[derive(Template)]
#[template(path = "_routine.html")]
pub(crate) struct RoutinesSection {
    pub date: NaiveDate,
    pub progress_done: u32,
    pub progress_total: u32,
    pub progress_percent: u32,
    pub has_routines: bool,
    pub rows: Vec<RowVm>,
    pub blocks: Vec<BlockVm>,
    pub now_label: String,
    pub now_at_end: bool,
    pub readonly: bool,
    pub locked: bool,
}

#[derive(Template)]
#[template(path = "_todo.html")]
pub(crate) struct TodosSection {
    pub date: NaiveDate,
    pub is_today: bool,
    pub overdue: Vec<TodoVm>,
    pub todos: Vec<TodoVm>,
    pub deadlines: Vec<TodoVm>,
    pub readonly: bool,
}

#[derive(Template)]
#[template(path = "_note.html")]
pub(crate) struct NoteSection {
    pub date: NaiveDate,
    pub body: Option<String>,
    pub saved: bool,
    pub readonly: bool,
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
    pub done_days: String,
    pub pin: Option<PinVm>,
}

pub(crate) fn done_days_label(dates: &[NaiveDate], period: crate::recurrence::PeriodKind) -> String {
    let format = match period {
        crate::recurrence::PeriodKind::Day => return String::new(),
        crate::recurrence::PeriodKind::Week => "%a",
        crate::recurrence::PeriodKind::Month | crate::recurrence::PeriodKind::Year => "%-d %b",
    };
    return dates
        .iter()
        .map(|date| date.format(format).to_string())
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
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
    pub col: u32,
    pub cols: u32,
    pub time_label: String,
    pub start_label: String,
    pub now_before: bool,
    pub pin: Option<PinVm>,
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
    pub is_deadline: bool,
    pub countdown: Option<String>,
    pub done: bool,
    pub overdue: bool,
}

pub(crate) struct TodoLists {
    pub overdue: Vec<TodoVm>,
    pub todos: Vec<TodoVm>,
    pub deadlines: Vec<TodoVm>,
}

struct RoutineData {
    progress_done: u32,
    progress_total: u32,
    has_routines: bool,
    rows: Vec<RowVm>,
    blocks: Vec<BlockVm>,
    now_at_end: bool,
}

struct PendingBlock {
    block: BlockVm,
    start: i64,
    end: i64,
}

#[derive(Debug, Deserialize)]
pub struct DayQuery {
    pub date: Option<String>,
    pub who: Option<String>,
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
    kind: String,
    all_day: bool,
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
    let user = super::current_user(&state, &headers).await?;
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
    let members = store::list_members(&state.pool).await?;
    let target = match query.who.as_deref().map(str::trim) {
        None | Some("") | Some("me") => auth.id,
        Some(raw) => match raw.parse::<i64>() {
            Ok(id) if members.iter().any(|member| member.id == id) => id,
            _ => return Err(WebError::NotFound),
        },
    };
    let readonly = target != auth.id;
    let mut conn = state.pool.acquire().await?;
    store::materialize(&mut conn, target, today).await?;
    drop(conn);
    let shell = Shell::load(&state, &auth, "calendar", &headers).await?;
    let now_min = local_now_minutes(&auth.timezone);
    let data = build_routine_data(&state.pool, target, date, today, now_min).await?;
    let lists = build_todo_lists(&state.pool, target, &auth.timezone).await?;
    let body = store::get_note(&state.pool, target, date).await?;
    let onboarding = if readonly {
        None
    } else {
        build_onboarding(&state.pool, auth.id).await?
    };
    let picker = super::week::build_picker("/day", auth.id, Some(target), date, &members);
    let (who_q, who_param) = if readonly {
        (format!("&who={target}"), target.to_string())
    } else {
        (String::new(), "me".to_string())
    };
    let progress_total = data.progress_total;
    let progress_percent = progress_percent(data.progress_done, progress_total);
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
        now_label: fmt_minutes(now_min),
        now_at_end: data.now_at_end,
        overdue: lists.overdue,
        todos: lists.todos,
        deadlines: lists.deadlines,
        body,
        saved: false,
        readonly,
        locked: date != today,
        picker,
        who_q,
        who_param,
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
            kind: t.kind.clone(),
            all_day: t.all_day,
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
    pool: &PgPool,
    user_id: i64,
    date: NaiveDate,
    today: NaiveDate,
    now_min: i64,
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
        if info.completed || (is_count && quota_met) {
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
                        col: 1,
                        cols: 1,
                        time_label: format!(
                            "{}\u{2013}{}",
                            fmt_minutes(start_min),
                            fmt_minutes(start_min + len_min)
                        ),
                        start_label: fmt_minutes(start_min),
                        now_before: false,
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
                let done_days = match (is_count, info.routine.period) {
                    (true, Some(period)) => match crate::recurrence::period_bounds(date, period) {
                        Some((start, _)) => done_days_label(
                            &store::period_done_dates(pool, info.routine.id, start).await?,
                            period,
                        ),
                        None => String::new(),
                    },
                    _ => String::new(),
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
                    done_days,
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
    let mut now_at_end = false;
    if date == today && !blocks.is_empty() {
        match blocks.iter_mut().find(|b| b.start_min > now_min) {
            Some(block) => block.now_before = true,
            None => now_at_end = true,
        }
    }
    return Ok(RoutineData {
        progress_done,
        progress_total,
        has_routines,
        rows,
        blocks,
        now_at_end,
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
    pool: &PgPool,
    user_id: i64,
    timezone: &str,
    date: NaiveDate,
    today: NaiveDate,
) -> Result<RoutinesSection, WebError> {
    let now_min = local_now_minutes(timezone);
    let data = build_routine_data(pool, user_id, date, today, now_min).await?;
    let progress_percent = progress_percent(data.progress_done, data.progress_total);
    return Ok(RoutinesSection {
        date,
        progress_done: data.progress_done,
        progress_total: data.progress_total,
        progress_percent,
        has_routines: data.has_routines,
        rows: data.rows,
        blocks: data.blocks,
        now_label: fmt_minutes(now_min),
        now_at_end: data.now_at_end,
        readonly: false,
        locked: date != today,
    });
}

pub(crate) async fn build_todos_section(
    pool: &PgPool,
    user_id: i64,
    timezone: &str,
    date: NaiveDate,
    today: NaiveDate,
) -> Result<TodosSection, WebError> {
    let lists = build_todo_lists(pool, user_id, timezone).await?;
    return Ok(TodosSection {
        date,
        is_today: date == today,
        overdue: lists.overdue,
        todos: lists.todos,
        deadlines: lists.deadlines,
        readonly: false,
    });
}

async fn build_todo_lists(
    pool: &PgPool,
    user_id: i64,
    timezone: &str,
) -> Result<TodoLists, WebError> {
    let now = Utc::now().to_rfc3339();
    let keep_done_after = (Utc::now() - chrono::Duration::hours(24)).to_rfc3339();
    let tz = store::user_timezone(timezone);
    let local_today = Utc::now().with_timezone(&tz).date_naive();
    let todo_rows = store::list_todos(pool, user_id, &now).await?;
    let mut overdue = Vec::new();
    let mut todos = Vec::new();
    let mut deadlines = Vec::new();
    for t in &todo_rows {
        if let Some(done_at) = &t.completed_at
            && done_at.as_str() < keep_done_after.as_str()
        {
            continue;
        }
        let vm = TodoVm {
            id: t.id,
            title: t.title.clone(),
            notes: t.notes.clone(),
            deadline: t
                .deadline
                .as_deref()
                .map(|raw| fmt_deadline(raw, &tz, local_today, t.all_day)),
            is_deadline: t.kind == "deadline",
            countdown: t
                .deadline
                .as_deref()
                .and_then(|raw| fmt_countdown(raw, &tz, local_today)),
            done: t.completed_at.is_some(),
            overdue: is_overdue(t, &now),
        };
        if vm.overdue {
            overdue.push(vm);
        } else if vm.is_deadline {
            deadlines.push(vm);
        } else {
            todos.push(vm);
        }
    }
    return Ok(TodoLists {
        overdue,
        todos,
        deadlines,
    });
}

async fn build_onboarding(
    pool: &PgPool,
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
    pool: &PgPool,
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

fn fmt_deadline(raw: &str, tz: &chrono_tz::Tz, today: NaiveDate, all_day: bool) -> String {
    return match DateTime::parse_from_rfc3339(raw) {
        Ok(parsed) => {
            let local = parsed.with_timezone(tz);
            let day = local.date_naive();
            let word = if day == today {
                "Today".to_string()
            } else if Some(day) == today.succ_opt() {
                "Tomorrow".to_string()
            } else if Some(day) == today.pred_opt() {
                "Yesterday".to_string()
            } else if day.year() == today.year() {
                local.format("%a %-d %b").to_string()
            } else {
                local.format("%a %-d %b %Y").to_string()
            };
            if all_day {
                return word;
            }
            return format!("{word} {}", local.format("%H:%M"));
        }
        Err(_) => raw.to_string(),
    };
}

fn fmt_countdown(raw: &str, tz: &chrono_tz::Tz, today: NaiveDate) -> Option<String> {
    let parsed = DateTime::parse_from_rfc3339(raw).ok()?;
    let days = (parsed.with_timezone(tz).date_naive() - today).num_days();
    return Some(match days {
        0 => "today".to_string(),
        1 => "tomorrow".to_string(),
        -1 => "yesterday".to_string(),
        d if d > 1 => format!("in {d} days"),
        d => format!("{} days ago", -d),
    });
}

fn progress_percent(done: u32, total: u32) -> u32 {
    if total == 0 {
        return 0;
    }
    return done * 100 / total;
}

pub(crate) fn local_now_minutes(timezone: &str) -> i64 {
    let now = Utc::now().with_timezone(&store::user_timezone(timezone));
    return i64::from(now.hour()) * 60 + i64::from(now.minute());
}
