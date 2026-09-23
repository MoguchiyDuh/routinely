use std::str::FromStr;

use chrono::Datelike;
use chrono::NaiveDate;
use chrono::Utc;
use chrono_tz::Tz;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnection;

use crate::recurrence::Schedule;
use crate::recurrence::effective_end;
use crate::recurrence::mask_contains;
use crate::recurrence::occurrences_in_range;
use crate::recurrence::period_bounds;
use crate::recurrence::periods_in_range;

pub use crate::recurrence::PeriodKind;
pub use crate::recurrence::ScheduleKind;

pub const UPLOADS_DIR: &str = "data/uploads";

#[derive(Debug, Clone)]
pub struct RoutineRow {
    pub id: i64,
    pub user_id: i64,
    pub title: String,
    pub notes: Option<String>,
    pub kind: String,
    pub weekdays_mask: Option<u8>,
    pub period: Option<PeriodKind>,
    pub per_period: Option<u32>,
    pub iterations: Option<u32>,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub time_of_day: Option<String>,
    pub time_end: Option<String>,
    pub archived_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DayRoutineInfo {
    pub routine: RoutineRow,
    pub occurrence_id: Option<i64>,
    pub completed: bool,
    pub evidence_note: Option<String>,
    pub status: &'static str,
    pub period_done: Option<u32>,
    pub period_required: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct WeekOccurrenceRow {
    pub occurrence_id: i64,
    pub routine_id: i64,
    pub title: String,
    pub time_of_day: Option<String>,
    pub time_end: Option<String>,
    pub username: String,
    pub due_date: NaiveDate,
    pub completed: bool,
    pub has_pin: bool,
}

#[derive(Debug, Clone)]
pub struct RoutineWithUser {
    pub routine: RoutineRow,
    pub username: String,
}

#[derive(Debug, Clone)]
pub struct WeekTodoRow {
    pub id: i64,
    pub title: String,
    pub username: String,
    pub deadline: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PinRow {
    pub occurrence_id: i64,
    pub summary: String,
    pub links: Vec<String>,
    pub photos: Vec<String>,
}

type WeekOccurrenceRecord = (
    i64,
    i64,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    bool,
    bool,
);

type RoutineWithUserRecord = (
    i64,
    i64,
    String,
    Option<String>,
    String,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

pub async fn week_occurrences(
    pool: &SqlitePool,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<WeekOccurrenceRow>, sqlx::Error> {
    let rows: Vec<WeekOccurrenceRecord> = sqlx::query_as(
        "SELECT o.id, r.id, r.title, r.time_of_day, r.time_end, u.username, o.due_date, \
         (c.id IS NOT NULL), (p.id IS NOT NULL) \
         FROM occurrences o \
         JOIN routines r ON r.id = o.routine_id \
         JOIN users u ON u.id = r.user_id \
         LEFT JOIN completions c ON c.occurrence_id = o.id \
         LEFT JOIN pins p ON p.completion_id = c.id \
         WHERE o.due_date >= ? AND o.due_date <= ? \
         ORDER BY o.due_date, o.id",
    )
    .bind(start.to_string())
    .bind(end.to_string())
    .fetch_all(pool)
    .await?;
    return Ok(rows
        .into_iter()
        .map(
            |(
                occurrence_id,
                routine_id,
                title,
                time_of_day,
                time_end,
                username,
                due_date,
                completed,
                has_pin,
            )| {
                WeekOccurrenceRow {
                    occurrence_id,
                    routine_id,
                    title,
                    time_of_day,
                    time_end,
                    username,
                    due_date: NaiveDate::parse_from_str(&due_date, "%Y-%m-%d").unwrap_or_default(),
                    completed,
                    has_pin,
                }
            },
        )
        .collect());
}

pub async fn active_routines_with_users(
    pool: &SqlitePool,
) -> Result<Vec<RoutineWithUser>, sqlx::Error> {
    let rows: Vec<RoutineWithUserRecord> = sqlx::query_as(
        "SELECT r.id, r.user_id, r.title, r.notes, r.kind, r.weekdays_mask, r.period, r.per_period, \
         r.iterations, r.start_date, r.end_date, r.time_of_day, r.time_end, r.archived_at, u.username \
         FROM routines r JOIN users u ON u.id = r.user_id \
         WHERE r.archived_at IS NULL ORDER BY r.id",
    )
    .fetch_all(pool)
    .await?;
    let mut out = Vec::new();
    for (
        id,
        user_id,
        title,
        notes,
        kind,
        weekdays_mask,
        period,
        per_period,
        iterations,
        start_date,
        end_date,
        time_of_day,
        time_end,
        archived_at,
        username,
    ) in rows
    {
        out.push(RoutineWithUser {
            routine: routine_from_row((
                id,
                user_id,
                title,
                notes,
                kind,
                weekdays_mask,
                period,
                per_period,
                iterations,
                start_date,
                end_date,
                time_of_day,
                time_end,
                archived_at,
            )),
            username,
        });
    }
    return Ok(out);
}

type WeekTodoRecord = (i64, String, Option<String>, Option<String>, String);

type PinAttachmentRecord = (i64, String, Option<String>, Option<String>, Option<String>);

pub async fn todos_with_deadline_between(
    pool: &SqlitePool,
    from_utc: &str,
    until_utc_exclusive: &str,
) -> Result<Vec<WeekTodoRow>, sqlx::Error> {
    let rows: Vec<WeekTodoRecord> = sqlx::query_as(
        "SELECT t.id, t.title, t.deadline, t.completed_at, u.username \
         FROM todos t JOIN users u ON u.id = t.user_id \
         WHERE t.deadline IS NOT NULL AND t.deadline >= ? AND t.deadline < ? \
         ORDER BY t.deadline, t.id",
    )
    .bind(from_utc)
    .bind(until_utc_exclusive)
    .fetch_all(pool)
    .await?;
    return Ok(rows
        .into_iter()
        .map(
            |(id, title, deadline, completed_at, username)| WeekTodoRow {
                id,
                title,
                username,
                deadline: deadline.unwrap_or_default(),
                completed_at,
            },
        )
        .collect());
}

pub async fn period_completion_counts(
    pool: &SqlitePool,
) -> Result<Vec<(i64, String, i64)>, sqlx::Error> {
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT o.routine_id, o.period_start, COUNT(*) \
         FROM occurrences o JOIN completions c ON c.occurrence_id = o.id \
         WHERE o.period_start IS NOT NULL \
         GROUP BY o.routine_id, o.period_start",
    )
    .fetch_all(pool)
    .await?;
    return Ok(rows);
}

pub async fn pins_for_occurrences(
    pool: &SqlitePool,
    occurrence_ids: &[i64],
) -> Result<Vec<PinRow>, sqlx::Error> {
    if occurrence_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut builder = sqlx::QueryBuilder::new(
        "SELECT c.occurrence_id, p.summary, a.kind, a.url, a.file_path \
         FROM pins p \
         JOIN completions c ON c.id = p.completion_id \
         LEFT JOIN attachments a ON a.pin_id = p.id \
         WHERE c.occurrence_id IN (",
    );
    let mut separated = builder.separated(", ");
    for id in occurrence_ids {
        separated.push_bind(id);
    }
    builder.push(") ORDER BY c.occurrence_id, a.id");
    let rows: Vec<PinAttachmentRecord> = builder.build_query_as().fetch_all(pool).await?;
    let mut out: Vec<PinRow> = Vec::new();
    for (occurrence_id, summary, kind, url, file_path) in rows {
        if out
            .last()
            .is_none_or(|last| last.occurrence_id != occurrence_id)
        {
            out.push(PinRow {
                occurrence_id,
                summary,
                links: Vec::new(),
                photos: Vec::new(),
            });
        }
        let Some(last) = out.last_mut() else {
            continue;
        };
        if kind.as_deref() == Some("link") {
            if let Some(url) = url {
                last.links.push(url);
            }
        } else if let Some(path) = file_path {
            last.photos.push(path);
        }
    }
    return Ok(out);
}

#[derive(Debug, Clone)]
pub struct TodoRow {
    pub id: i64,
    pub title: String,
    pub notes: Option<String>,
    pub deadline: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PinInfo {
    pub summary: String,
    pub links: Vec<String>,
    pub photos: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RoutineSpec {
    pub title: String,
    pub notes: Option<String>,
    pub kind: ScheduleKind,
    pub weekdays_mask: Option<u8>,
    pub period: Option<PeriodKind>,
    pub per_period: Option<u32>,
    pub iterations: Option<u32>,
    pub end_date: Option<NaiveDate>,
    pub time_of_day: Option<String>,
    pub time_end: Option<String>,
    pub start_date: NaiveDate,
}

pub fn user_timezone(tz: &str) -> Tz {
    return Tz::from_str(tz).unwrap_or(chrono_tz::UTC);
}

pub fn user_today(tz: &str) -> NaiveDate {
    return Utc::now().with_timezone(&user_timezone(tz)).date_naive();
}

pub fn parse_period(raw: &str) -> Option<PeriodKind> {
    return match raw {
        "day" => Some(PeriodKind::Day),
        "week" => Some(PeriodKind::Week),
        "month" => Some(PeriodKind::Month),
        "year" => Some(PeriodKind::Year),
        _ => None,
    };
}

pub fn schedule_from_row(row: &RoutineRow) -> Option<Schedule> {
    let start_date = row.start_date?;
    let kind = match row.kind.as_str() {
        "weekdays" => ScheduleKind::Weekdays,
        "count" => ScheduleKind::Count,
        _ => return None,
    };
    let period = row.period.unwrap_or(PeriodKind::Day);
    let per_period = row.per_period.unwrap_or(1);
    let end_date = row.end_date;
    if kind == ScheduleKind::Count && row.period.is_none() {
        return None;
    }
    return Some(Schedule {
        kind,
        weekdays_mask: row.weekdays_mask.unwrap_or(0),
        period,
        per_period,
        iterations: row.iterations,
        start_date,
        end_date,
    });
}

type RoutineRecord = (
    i64,
    i64,
    String,
    Option<String>,
    String,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

type TodoRecord = (i64, String, Option<String>, Option<String>, Option<String>);

fn routine_from_row(row: RoutineRecord) -> RoutineRow {
    return RoutineRow {
        id: row.0,
        user_id: row.1,
        title: row.2,
        notes: row.3,
        kind: row.4,
        weekdays_mask: row.5.and_then(|v| u8::try_from(v).ok()),
        period: row.6.and_then(|v| parse_period(&v)),
        per_period: row.7.and_then(|v| u32::try_from(v).ok()),
        iterations: row.8.and_then(|v| u32::try_from(v).ok()),
        start_date: NaiveDate::parse_from_str(&row.9, "%Y-%m-%d").ok(),
        end_date: row
            .10
            .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
        time_of_day: row.11,
        time_end: row.12,
        archived_at: row.13,
    };
}

pub async fn routine_for_user(
    pool: &SqlitePool,
    user_id: i64,
    routine_id: i64,
) -> Result<Option<RoutineRow>, sqlx::Error> {
    let row = sqlx::query_as::<_, RoutineRecord>(
        "SELECT id, user_id, title, notes, kind, weekdays_mask, period, per_period, iterations, start_date, end_date, time_of_day, time_end, archived_at \
         FROM routines WHERE id = ? AND user_id = ?",
    )
    .bind(routine_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    return Ok(row.map(routine_from_row));
}

pub async fn active_routines(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Vec<RoutineRow>, sqlx::Error> {
    let rows = sqlx::query_as::<_, RoutineRecord>(
        "SELECT id, user_id, title, notes, kind, weekdays_mask, period, per_period, iterations, start_date, end_date, time_of_day, time_end, archived_at \
         FROM routines WHERE user_id = ? AND archived_at IS NULL ORDER BY id",
    )
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await?;
    return Ok(rows.into_iter().map(routine_from_row).collect());
}

pub async fn create_routine(
    pool: &SqlitePool,
    user_id: i64,
    spec: &RoutineSpec,
) -> Result<i64, sqlx::Error> {
    let kind = match spec.kind {
        ScheduleKind::Weekdays => "weekdays",
        ScheduleKind::Count => "count",
    };
    let period = spec.period.map(|p| match p {
        PeriodKind::Day => "day",
        PeriodKind::Week => "week",
        PeriodKind::Month => "month",
        PeriodKind::Year => "year",
    });
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO routines (user_id, title, notes, kind, weekdays_mask, period, per_period, iterations, start_date, end_date, time_of_day, time_end, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user_id)
    .bind(&spec.title)
    .bind(&spec.notes)
    .bind(kind)
    .bind(spec.weekdays_mask.map(i64::from))
    .bind(period)
    .bind(spec.per_period.map(i64::from))
    .bind(spec.iterations.map(i64::from))
    .bind(spec.start_date.to_string())
    .bind(spec.end_date.map(|d| d.to_string()))
    .bind(&spec.time_of_day)
    .bind(&spec.time_end)
    .bind(Utc::now().to_rfc3339())
    .fetch_one(pool)
    .await?;
    return Ok(id);
}

pub async fn archive_routine(
    pool: &SqlitePool,
    user_id: i64,
    routine_id: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE routines SET archived_at = ? WHERE id = ? AND user_id = ?")
        .bind(Utc::now().to_rfc3339())
        .bind(routine_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    return Ok(result.rows_affected() > 0);
}

pub async fn materialize(
    conn: &mut SqliteConnection,
    user_id: i64,
    today: NaiveDate,
) -> Result<(), sqlx::Error> {
    let routines = active_routines(&mut *conn, user_id).await?;
    let now = Utc::now().to_rfc3339();
    for routine in &routines {
        let Some(schedule) = schedule_from_row(routine) else {
            continue;
        };
        if schedule.kind != ScheduleKind::Weekdays {
            continue;
        }
        for date in occurrences_in_range(&schedule, schedule.start_date, today) {
            sqlx::query(
                "INSERT OR IGNORE INTO occurrences (routine_id, due_date, period_start, created_at) VALUES (?, ?, NULL, ?)",
            )
            .bind(routine.id)
            .bind(date.to_string())
            .bind(&now)
            .execute(&mut *conn)
            .await?;
        }
    }
    return Ok(());
}

fn is_scheduled_on(schedule: &Schedule, date: NaiveDate) -> bool {
    if date < schedule.start_date {
        return false;
    }
    return match schedule.kind {
        ScheduleKind::Weekdays => {
            mask_contains(schedule.weekdays_mask, date.weekday())
                && match effective_end(schedule) {
                    Some(end) => date <= end,
                    None => true,
                }
        }
        ScheduleKind::Count => !periods_in_range(schedule, date, date).is_empty(),
    };
}

pub async fn find_or_create_occurrence(
    pool: &SqlitePool,
    routine: &RoutineRow,
    day: NaiveDate,
) -> Result<i64, sqlx::Error> {
    let period_start = match routine.kind.as_str() {
        "count" => match routine.period {
            Some(period) => period_bounds(day, period).map(|(start, _)| start),
            None => None,
        },
        _ => None,
    };
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT OR IGNORE INTO occurrences (routine_id, due_date, period_start, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(routine.id)
    .bind(day.to_string())
    .bind(period_start.map(|d| d.to_string()))
    .bind(&now)
    .execute(pool)
    .await?;
    let id: i64 =
        sqlx::query_scalar("SELECT id FROM occurrences WHERE routine_id = ? AND due_date = ?")
            .bind(routine.id)
            .bind(day.to_string())
            .fetch_one(pool)
            .await?;
    return Ok(id);
}

pub async fn tick(
    pool: &SqlitePool,
    routine: &RoutineRow,
    day: NaiveDate,
    evidence_note: Option<&str>,
) -> Result<i64, sqlx::Error> {
    let occurrence_id = find_or_create_occurrence(pool, routine, day).await?;
    sqlx::query(
        "INSERT OR IGNORE INTO completions (occurrence_id, completed_at, evidence_note) VALUES (?, ?, ?)",
    )
    .bind(occurrence_id)
    .bind(Utc::now().to_rfc3339())
    .bind(evidence_note)
    .execute(pool)
    .await?;
    return Ok(occurrence_id);
}

pub async fn untick(pool: &SqlitePool, routine_id: i64, day: NaiveDate) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM completions WHERE occurrence_id IN (SELECT id FROM occurrences WHERE routine_id = ? AND due_date = ?)",
    )
    .bind(routine_id)
    .bind(day.to_string())
    .execute(pool)
    .await?;
    return Ok(());
}

pub async fn day_routines(
    pool: &SqlitePool,
    user_id: i64,
    date: NaiveDate,
    today: NaiveDate,
) -> Result<Vec<DayRoutineInfo>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let routines = active_routines(&mut conn, user_id).await?;
    let mut out = Vec::new();
    for routine in &routines {
        let Some(schedule) = schedule_from_row(routine) else {
            continue;
        };
        if !is_scheduled_on(&schedule, date) {
            continue;
        }
        let occurrence_id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM occurrences WHERE routine_id = ? AND due_date = ?")
                .bind(routine.id)
                .bind(date.to_string())
                .fetch_optional(pool)
                .await?;
        let completion: Option<(i64, Option<String>)> = match occurrence_id {
            Some(id) => {
                sqlx::query_as("SELECT id, evidence_note FROM completions WHERE occurrence_id = ?")
                    .bind(id)
                    .fetch_optional(pool)
                    .await?
            }
            None => None,
        };
        let completed = completion.is_some();
        let status = if completed {
            "completed"
        } else if date < today {
            "missed"
        } else {
            "open"
        };
        let (period_done, period_required) = if schedule.kind == ScheduleKind::Count {
            let required = schedule.per_period;
            let done = match period_bounds(date, schedule.period) {
                Some((start, _)) => {
                    let (count,): (i64,) = sqlx::query_as(
                        "SELECT COUNT(*) FROM completions c JOIN occurrences o ON o.id = c.occurrence_id \
                         WHERE o.routine_id = ? AND o.period_start = ?",
                    )
                    .bind(routine.id)
                    .bind(start.to_string())
                    .fetch_one(pool)
                    .await?;
                    u32::try_from(count).unwrap_or(0)
                }
                None => 0,
            };
            (Some(done), Some(required))
        } else {
            (None, None)
        };
        out.push(DayRoutineInfo {
            routine: routine.clone(),
            occurrence_id,
            completed,
            evidence_note: completion.and_then(|(_, note)| note),
            status,
            period_done,
            period_required,
        });
    }
    return Ok(out);
}

pub async fn occurrence_owned(
    pool: &SqlitePool,
    user_id: i64,
    occurrence_id: i64,
) -> Result<bool, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT o.id FROM occurrences o JOIN routines r ON r.id = o.routine_id \
         WHERE o.id = ? AND r.user_id = ?",
    )
    .bind(occurrence_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    return Ok(row.is_some());
}

pub async fn completion_id_for_occurrence(
    pool: &SqlitePool,
    occurrence_id: i64,
) -> Result<Option<i64>, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT id FROM completions WHERE occurrence_id = ?")
        .bind(occurrence_id)
        .fetch_optional(pool)
        .await?;
    return Ok(row.map(|(id,)| id));
}

pub async fn upsert_pin(
    pool: &SqlitePool,
    completion_id: i64,
    summary: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query(
        "INSERT INTO pins (completion_id, summary, created_at) VALUES (?, ?, ?) \
         ON CONFLICT (completion_id) DO UPDATE SET summary = excluded.summary",
    )
    .bind(completion_id)
    .bind(summary)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    let id: i64 = sqlx::query_scalar("SELECT id FROM pins WHERE completion_id = ?")
        .bind(completion_id)
        .fetch_one(pool)
        .await?;
    return Ok(id);
}

pub async fn add_link_attachment(
    pool: &SqlitePool,
    pin_id: i64,
    url: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO attachments (pin_id, kind, url, file_path, created_at) VALUES (?, 'link', ?, NULL, ?)",
    )
    .bind(pin_id)
    .bind(url)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    return Ok(());
}

pub async fn add_photo_attachment(
    pool: &SqlitePool,
    pin_id: i64,
    file_path: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO attachments (pin_id, kind, url, file_path, created_at) VALUES (?, 'photo', NULL, ?, ?)",
    )
    .bind(pin_id)
    .bind(file_path)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    return Ok(());
}

pub async fn pin_for_occurrence(
    pool: &SqlitePool,
    occurrence_id: i64,
) -> Result<Option<PinInfo>, sqlx::Error> {
    let pin: Option<(i64, String)> = sqlx::query_as(
        "SELECT p.id, p.summary FROM pins p JOIN completions c ON c.id = p.completion_id \
         WHERE c.occurrence_id = ?",
    )
    .bind(occurrence_id)
    .fetch_optional(pool)
    .await?;
    let Some((pin_id, summary)) = pin else {
        return Ok(None);
    };
    let rows: Vec<(String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT kind, url, file_path FROM attachments WHERE pin_id = ? ORDER BY id")
            .bind(pin_id)
            .fetch_all(pool)
            .await?;
    let mut links = Vec::new();
    let mut photos = Vec::new();
    for (kind, url, file_path) in rows {
        if kind == "link" {
            if let Some(url) = url {
                links.push(url);
            }
        } else if let Some(path) = file_path {
            photos.push(path);
        }
    }
    return Ok(Some(PinInfo {
        summary,
        links,
        photos,
    }));
}

pub async fn create_todo(
    pool: &SqlitePool,
    user_id: i64,
    title: &str,
    notes: Option<&str>,
    deadline: Option<&str>,
) -> Result<i64, sqlx::Error> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO todos (user_id, title, notes, deadline, completed_at, created_at) VALUES (?, ?, ?, ?, NULL, ?) RETURNING id",
    )
    .bind(user_id)
    .bind(title)
    .bind(notes)
    .bind(deadline)
    .bind(Utc::now().to_rfc3339())
    .fetch_one(pool)
    .await?;
    return Ok(id);
}

pub async fn list_todos(
    pool: &SqlitePool,
    user_id: i64,
    now: &str,
) -> Result<Vec<TodoRow>, sqlx::Error> {
    let rows: Vec<TodoRecord> = sqlx::query_as(
        "SELECT id, title, notes, deadline, completed_at FROM todos \
         WHERE user_id = ? \
         ORDER BY \
           CASE WHEN completed_at IS NULL AND deadline IS NOT NULL AND deadline < ? THEN 0 \
                WHEN completed_at IS NULL THEN 1 \
                ELSE 2 END, \
           CASE WHEN completed_at IS NULL AND deadline IS NULL THEN 1 ELSE 0 END, \
           CASE WHEN completed_at IS NULL THEN deadline END",
    )
    .bind(user_id)
    .bind(now)
    .fetch_all(pool)
    .await?;
    return Ok(rows
        .into_iter()
        .map(|(id, title, notes, deadline, completed_at)| TodoRow {
            id,
            title,
            notes,
            deadline,
            completed_at,
        })
        .collect());
}

pub async fn toggle_todo(
    pool: &SqlitePool,
    user_id: i64,
    todo_id: i64,
) -> Result<bool, sqlx::Error> {
    let result =
        sqlx::query("UPDATE todos SET completed_at = CASE WHEN completed_at IS NULL THEN ? ELSE NULL END WHERE id = ? AND user_id = ?")
            .bind(Utc::now().to_rfc3339())
            .bind(todo_id)
            .bind(user_id)
            .execute(pool)
            .await?;
    return Ok(result.rows_affected() > 0);
}

pub async fn delete_todo(
    pool: &SqlitePool,
    user_id: i64,
    todo_id: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM todos WHERE id = ? AND user_id = ?")
        .bind(todo_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    return Ok(result.rows_affected() > 0);
}

pub async fn upsert_note(
    pool: &SqlitePool,
    user_id: i64,
    day: NaiveDate,
    body: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO daily_notes (user_id, day, body, updated_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT (user_id, day) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
    )
    .bind(user_id)
    .bind(day.to_string())
    .bind(body)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    return Ok(());
}

pub async fn get_note(
    pool: &SqlitePool,
    user_id: i64,
    day: NaiveDate,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT body FROM daily_notes WHERE user_id = ? AND day = ?")
            .bind(user_id)
            .bind(day.to_string())
            .fetch_optional(pool)
            .await?;
    return Ok(row.map(|(body,)| body));
}
