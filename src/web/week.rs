use std::collections::HashMap;
use std::collections::HashSet;

use askama::Template;
use axum::Router;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use chrono::DateTime;
use chrono::LocalResult;
use chrono::NaiveDate;
use chrono::TimeZone;
use chrono::Utc;
use chrono_tz::Tz;
use serde::Deserialize;
use serde::Serialize;
use sqlx::SqlitePool;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::recurrence::PeriodKind;
use crate::recurrence::ScheduleKind;
use crate::recurrence::occurrences_in_range;
use crate::recurrence::period_bounds;
use crate::recurrence::periods_in_range;
use crate::store;

use axum::routing::get;

use super::WebError;
use super::WebResult;
use super::html;
use super::parse_date_param;
use super::shell::Shell;

#[derive(Template)]
#[template(path = "week.html")]
pub struct WeekPage {
    pub shell: Shell,
    week_start: NaiveDate,
    week_end: NaiveDate,
    prev_week: NaiveDate,
    next_week: NaiveDate,
    days: Vec<DayCell>,
}

#[derive(Template)]
#[template(path = "_pin.html")]
struct PinFragment {
    occurrence_id: i64,
    pin: WeekPin,
}

#[derive(Serialize)]
struct WeekJson {
    week_start: NaiveDate,
    week_end: NaiveDate,
    days: Vec<DayCell>,
}

#[derive(Serialize)]
struct DayCell {
    date: NaiveDate,
    entries: Vec<WeekEntry>,
}

#[derive(Serialize)]
#[serde(tag = "kind")]
enum WeekEntry {
    #[serde(rename = "occurrence")]
    Occurrence(OccurrenceEntry),
    #[serde(rename = "period")]
    Period(PeriodEntry),
    #[serde(rename = "todo")]
    Todo(TodoEntry),
}

#[derive(Serialize)]
struct OccurrenceEntry {
    occurrence_id: Option<i64>,
    routine_id: i64,
    title: String,
    username: String,
    status: &'static str,
    planned: bool,
    time_of_day: Option<String>,
    time_end: Option<String>,
    time_label: Option<String>,
    pin: Option<WeekPin>,
}

#[derive(Serialize)]
struct PeriodEntry {
    routine_id: i64,
    title: String,
    username: String,
    cell_date: NaiveDate,
    period_start: NaiveDate,
    period_end: NaiveDate,
    done: u32,
    required: u32,
}

#[derive(Serialize)]
struct TodoEntry {
    todo_id: i64,
    title: String,
    username: String,
    deadline: String,
    local_time: String,
    overdue: bool,
    completed: bool,
}

#[derive(Serialize)]
struct WeekPin {
    summary: String,
    links: Vec<String>,
    photos: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WeekQuery {
    date: Option<String>,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/week", get(week_view))
        .route("/api/v1/week", get(api_week));
}

pub(crate) async fn pin_fragment(
    PageAuth(_auth): PageAuth,
    State(state): State<AppState>,
    Path(occurrence_id): Path<i64>,
) -> WebResult {
    let Some(pin) = store::pin_for_occurrence(&state.pool, occurrence_id).await? else {
        return Err(WebError::NotFound);
    };
    return Ok(html(&PinFragment {
        occurrence_id,
        pin: WeekPin {
            summary: pin.summary,
            links: pin.links,
            photos: pin.photos.iter().map(|path| upload_url(path)).collect(),
        },
    })?);
}

async fn build_week(
    pool: &SqlitePool,
    timezone: &str,
    anchor: NaiveDate,
    today: NaiveDate,
) -> Result<(NaiveDate, NaiveDate, Vec<DayCell>), WebError> {
    let Some((week_start, week_end)) = period_bounds(anchor, PeriodKind::Week) else {
        return Err(WebError::Validation("invalid date"));
    };
    let tz = store::user_timezone(timezone);
    let mut by_date: HashMap<NaiveDate, Vec<WeekEntry>> = HashMap::new();
    let occurrences = store::week_occurrences(pool, week_start, week_end).await?;
    let mut db_keys: HashSet<(i64, NaiveDate)> = HashSet::new();
    let mut pinned: Vec<i64> = Vec::new();
    for occ in &occurrences {
        db_keys.insert((occ.routine_id, occ.due_date));
        if occ.has_pin {
            pinned.push(occ.occurrence_id);
        }
    }
    let mut pins: HashMap<i64, store::PinRow> = store::pins_for_occurrences(pool, &pinned)
        .await?
        .into_iter()
        .map(|row| (row.occurrence_id, row))
        .collect();
    for occ in occurrences {
        let pin = pins.remove(&occ.occurrence_id).map(|row| WeekPin {
            summary: row.summary,
            links: row.links,
            photos: row.photos.iter().map(|path| upload_url(path)).collect(),
        });
        let status = occurrence_status(occ.completed, occ.due_date, today);
        let time_label = time_label(occ.time_of_day.clone(), occ.time_end.clone());
        by_date
            .entry(occ.due_date)
            .or_default()
            .push(WeekEntry::Occurrence(OccurrenceEntry {
                occurrence_id: Some(occ.occurrence_id),
                routine_id: occ.routine_id,
                title: occ.title,
                username: occ.username,
                status,
                planned: false,
                time_of_day: occ.time_of_day,
                time_end: occ.time_end,
                time_label,
                pin,
            }));
    }
    let mut period_counts: HashMap<(i64, String), u32> = HashMap::new();
    for (routine_id, period_start, count) in store::period_completion_counts(pool).await? {
        period_counts.insert(
            (routine_id, period_start),
            u32::try_from(count).unwrap_or(0),
        );
    }
    for holder in store::active_routines_with_users(pool).await? {
        let Some(schedule) = store::schedule_from_row(&holder.routine) else {
            continue;
        };
        match schedule.kind {
            ScheduleKind::Weekdays => {
                for date in occurrences_in_range(&schedule, week_start, week_end) {
                    if db_keys.contains(&(holder.routine.id, date)) {
                        continue;
                    }
                    let status = occurrence_status(false, date, today);
                    let time_label = time_label(
                        holder.routine.time_of_day.clone(),
                        holder.routine.time_end.clone(),
                    );
                    by_date
                        .entry(date)
                        .or_default()
                        .push(WeekEntry::Occurrence(OccurrenceEntry {
                            occurrence_id: None,
                            routine_id: holder.routine.id,
                            title: holder.routine.title.clone(),
                            username: holder.username.clone(),
                            status,
                            planned: true,
                            time_of_day: holder.routine.time_of_day.clone(),
                            time_end: holder.routine.time_end.clone(),
                            time_label,
                            pin: None,
                        }));
                }
            }
            ScheduleKind::Count => {
                for period in periods_in_range(&schedule, week_start, week_end) {
                    let done = period_counts
                        .get(&(holder.routine.id, period.period_start.to_string()))
                        .copied()
                        .unwrap_or(0);
                    if done >= period.required {
                        continue;
                    }
                    let cell_date = period.period_start.max(week_start);
                    by_date
                        .entry(cell_date)
                        .or_default()
                        .push(WeekEntry::Period(PeriodEntry {
                            routine_id: holder.routine.id,
                            title: holder.routine.title.clone(),
                            username: holder.username.clone(),
                            cell_date,
                            period_start: period.period_start,
                            period_end: period.period_end,
                            done,
                            required: period.required,
                        }));
                }
            }
        }
    }
    let range_start = local_midnight_utc(tz, week_start).to_rfc3339();
    let range_end = match week_end.succ_opt() {
        Some(next) => local_midnight_utc(tz, next).to_rfc3339(),
        None => local_midnight_utc(tz, week_end).to_rfc3339(),
    };
    let now = Utc::now();
    for todo in store::todos_with_deadline_between(pool, &range_start, &range_end).await? {
        let Ok(deadline) = DateTime::parse_from_rfc3339(&todo.deadline) else {
            continue;
        };
        let local = deadline.with_timezone(&tz);
        let day = local.date_naive();
        if day < week_start || day > week_end {
            continue;
        }
        let completed = todo.completed_at.is_some();
        let overdue = !completed && deadline.with_timezone(&Utc) < now;
        by_date
            .entry(day)
            .or_default()
            .push(WeekEntry::Todo(TodoEntry {
                todo_id: todo.id,
                title: todo.title,
                username: todo.username,
                deadline: todo.deadline,
                local_time: local.format("%H:%M").to_string(),
                overdue,
                completed,
            }));
    }
    let mut days: Vec<DayCell> = Vec::new();
    let mut cursor = week_start;
    while cursor <= week_end {
        let mut entries = by_date.remove(&cursor).unwrap_or_default();
        entries.sort_by(compare_entries);
        days.push(DayCell {
            date: cursor,
            entries,
        });
        cursor = match cursor.succ_opt() {
            Some(next) => next,
            None => break,
        };
    }
    return Ok((week_start, week_end, days));
}

async fn week_view(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Query(query): Query<WeekQuery>,
    headers: axum::http::HeaderMap,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    let anchor = parse_date_param(query.date.as_deref(), today)?;
    let shell = Shell::load(&state, &auth, "week", &headers).await?;
    let (week_start, week_end, days) =
        build_week(&state.pool, &auth.timezone, anchor, today).await?;
    let (prev_week, next_week) = neighbor_weeks(week_start, week_end);
    let page = WeekPage {
        shell,
        week_start,
        week_end,
        prev_week,
        next_week,
        days,
    };
    return Ok(html(&page)?);
}

async fn api_week(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Query(query): Query<WeekQuery>,
) -> Result<axum::Json<WeekJson>, WebError> {
    let today = store::user_today(&auth.timezone);
    let anchor = parse_date_param(query.date.as_deref(), today)?;
    let (week_start, week_end, days) =
        build_week(&state.pool, &auth.timezone, anchor, today).await?;
    return Ok(axum::Json(WeekJson {
        week_start,
        week_end,
        days,
    }));
}

fn neighbor_weeks(week_start: NaiveDate, week_end: NaiveDate) -> (NaiveDate, NaiveDate) {
    let prev = week_start
        .pred_opt()
        .and_then(|date| period_bounds(date, PeriodKind::Week))
        .map(|(start, _)| start)
        .unwrap_or(week_start);
    let next = week_end
        .succ_opt()
        .and_then(|date| period_bounds(date, PeriodKind::Week))
        .map(|(start, _)| start)
        .unwrap_or(week_start);
    return (prev, next);
}

fn occurrence_status(completed: bool, date: NaiveDate, today: NaiveDate) -> &'static str {
    if completed {
        return "completed";
    }
    if date < today {
        return "missed";
    }
    return "open";
}

fn time_label(start: Option<String>, end: Option<String>) -> Option<String> {
    let start = start?;
    return match end {
        Some(end) => Some(format!("{start}\u{2013}{end}")),
        None => Some(start),
    };
}

fn upload_url(file_path: &str) -> String {
    return format!("/uploads/{file_path}");
}

fn local_midnight_utc(tz: Tz, date: NaiveDate) -> DateTime<Utc> {
    let naive = match date.and_hms_opt(0, 0, 0) {
        Some(value) => value,
        None => return Utc::now(),
    };
    let local = match tz.from_local_datetime(&naive) {
        LocalResult::Single(value) => value,
        LocalResult::Ambiguous(value, _) => value,
        LocalResult::None => return Utc.from_utc_datetime(&naive),
    };
    return local.with_timezone(&Utc);
}

fn entry_rank(entry: &WeekEntry) -> u8 {
    return match entry {
        WeekEntry::Occurrence(_) => 0,
        WeekEntry::Period(_) => 1,
        WeekEntry::Todo(_) => 2,
    };
}

fn compare_entries(a: &WeekEntry, b: &WeekEntry) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let rank = entry_rank(a).cmp(&entry_rank(b));
    if rank != Ordering::Equal {
        return rank;
    }
    return match (a, b) {
        (WeekEntry::Occurrence(x), WeekEntry::Occurrence(y)) => {
            let key_x = (
                x.time_of_day.is_none(),
                x.time_of_day.as_deref().unwrap_or(""),
                x.title.as_str(),
            );
            let key_y = (
                y.time_of_day.is_none(),
                y.time_of_day.as_deref().unwrap_or(""),
                y.title.as_str(),
            );
            key_x.cmp(&key_y)
        }
        (WeekEntry::Period(x), WeekEntry::Period(y)) => {
            (x.cell_date, x.routine_id).cmp(&(y.cell_date, y.routine_id))
        }
        (WeekEntry::Todo(x), WeekEntry::Todo(y)) => x.deadline.cmp(&y.deadline),
        _ => Ordering::Equal,
    };
}
