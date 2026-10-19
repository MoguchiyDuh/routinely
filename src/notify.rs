use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

use chrono::DateTime;
use chrono::Days;
use chrono::Duration;
use chrono::LocalResult;
use chrono::NaiveDate;
use chrono::NaiveTime;
use chrono::TimeZone;
use chrono::Utc;
use chrono_tz::Tz;
use serde::Deserialize;
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::auth::session::SessionUser;
use crate::punish;
use crate::recurrence::Schedule;
use crate::recurrence::ScheduleKind;
use crate::recurrence::occurrences_in_range;
use crate::recurrence::period_bounds;
use crate::recurrence::periods_in_range;
use crate::store;

pub const TICK_INTERVAL: StdDuration = StdDuration::from_secs(30);
pub const FIRE_WINDOW_MINUTES: i64 = 15;
pub const DEFAULT_PAGE_SIZE: i64 = 30;
pub const MAX_PAGE_SIZE: i64 = 100;
pub const READ_RETENTION_DAYS: i64 = 30;
pub const UNREAD_RETENTION_DAYS: i64 = 90;
pub const PENALTY_LOOKBACK_HOURS: i64 = 24;
const DAY_END_LIST_CAP: usize = 5;

static LAST_PURGE_DAY: AtomicI64 = AtomicI64::new(i64::MIN);

#[derive(Debug, Clone, Serialize)]
pub struct NotificationRow {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub body: Option<String>,
    pub url: Option<String>,
    pub created_at: String,
    pub read_at: Option<String>,
    pub unread: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationPrefs {
    pub enabled: bool,
    pub day_end_enabled: bool,
    pub day_end_time: String,
    pub penalty_enabled: bool,
    pub quiet_start: Option<String>,
    pub quiet_end: Option<String>,
    pub default_routine_timed_minutes: Option<i64>,
    pub default_routine_allday_enabled: bool,
    pub default_routine_allday_time: String,
    pub default_todo_timed_minutes: Option<i64>,
    pub default_todo_allday_days: Option<i64>,
    pub default_todo_allday_time: String,
}

impl Default for NotificationPrefs {
    fn default() -> Self {
        return NotificationPrefs {
            enabled: true,
            day_end_enabled: true,
            day_end_time: "20:00".to_string(),
            penalty_enabled: true,
            quiet_start: None,
            quiet_end: None,
            default_routine_timed_minutes: Some(10),
            default_routine_allday_enabled: false,
            default_routine_allday_time: "09:00".to_string(),
            default_todo_timed_minutes: Some(60),
            default_todo_allday_days: Some(1),
            default_todo_allday_time: "09:00".to_string(),
        };
    }
}

type NotificationRecord = (
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

type PrefsRecord = (
    i64,
    i64,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<i64>,
    i64,
    String,
    Option<i64>,
    Option<i64>,
    String,
);

const PREFS_COLUMNS: &str = "enabled, day_end_enabled, day_end_time, penalty_enabled, quiet_start, quiet_end, \
     default_routine_timed_minutes, default_routine_allday_enabled, default_routine_allday_time, \
     default_todo_timed_minutes, default_todo_allday_days, default_todo_allday_time";

type ReminderTickRecord = (i64, i64, String, i64, String, Option<String>);

type TodoReminderRecord = (
    i64,
    String,
    i64,
    String,
    Option<String>,
    i64,
    String,
    String,
    i64,
);

fn notification_from_record(row: NotificationRecord) -> NotificationRow {
    return NotificationRow {
        id: row.0,
        kind: row.1,
        title: row.2,
        body: row.3,
        url: row.4,
        created_at: row.5,
        unread: row.6.is_none(),
        read_at: row.6,
    };
}

pub async fn unread_count(pool: &SqlitePool, user_id: i64) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND read_at IS NULL",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    return Ok(count);
}

pub async fn list(
    pool: &SqlitePool,
    user_id: i64,
    limit: i64,
    before_id: Option<i64>,
) -> Result<Vec<NotificationRow>, sqlx::Error> {
    let rows: Vec<NotificationRecord> = sqlx::query_as(
        "SELECT id, kind, title, body, url, created_at, read_at FROM notifications \
         WHERE user_id = $1 AND ($2 IS NULL OR id < $2) ORDER BY id DESC LIMIT $3",
    )
    .bind(user_id)
    .bind(before_id)
    .bind(limit.clamp(1, MAX_PAGE_SIZE))
    .fetch_all(pool)
    .await?;
    return Ok(rows.into_iter().map(notification_from_record).collect());
}

pub async fn find(
    pool: &SqlitePool,
    user_id: i64,
    notification_id: i64,
) -> Result<Option<NotificationRow>, sqlx::Error> {
    let row: Option<NotificationRecord> = sqlx::query_as(
        "SELECT id, kind, title, body, url, created_at, read_at FROM notifications \
         WHERE id = $1 AND user_id = $2",
    )
    .bind(notification_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    return Ok(row.map(notification_from_record));
}

pub async fn mark_read(
    pool: &SqlitePool,
    user_id: i64,
    notification_id: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE notifications SET read_at = COALESCE(read_at, $1) WHERE id = $2 AND user_id = $3",
    )
    .bind(Utc::now().to_rfc3339())
    .bind(notification_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    return Ok(result.rows_affected() > 0);
}

pub async fn mark_all_read(pool: &SqlitePool, user_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE notifications SET read_at = $1 WHERE user_id = $2 AND read_at IS NULL",
    )
    .bind(Utc::now().to_rfc3339())
    .bind(user_id)
    .execute(pool)
    .await?;
    return Ok(result.rows_affected());
}

fn prefs_from_record(row: PrefsRecord) -> NotificationPrefs {
    return NotificationPrefs {
        enabled: row.0 != 0,
        day_end_enabled: row.1 != 0,
        day_end_time: row.2,
        penalty_enabled: row.3 != 0,
        quiet_start: row.4,
        quiet_end: row.5,
        default_routine_timed_minutes: row.6,
        default_routine_allday_enabled: row.7 != 0,
        default_routine_allday_time: row.8,
        default_todo_timed_minutes: row.9,
        default_todo_allday_days: row.10,
        default_todo_allday_time: row.11,
    };
}

pub async fn get_prefs(pool: &SqlitePool, user_id: i64) -> Result<NotificationPrefs, sqlx::Error> {
    let row: Option<PrefsRecord> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {PREFS_COLUMNS} FROM notification_prefs WHERE user_id = $1"
    )))
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    return Ok(row.map(prefs_from_record).unwrap_or_default());
}

async fn resolve_prefs(
    pool: &SqlitePool,
    user_id: i64,
    now_utc: DateTime<Utc>,
) -> Result<(NotificationPrefs, DateTime<Utc>), sqlx::Error> {
    insert_prefs_row(pool, user_id, &NotificationPrefs::default(), now_utc, true).await?;
    let prefs = get_prefs(pool, user_id).await?;
    let (since,): (Option<String>,) =
        sqlx::query_as("SELECT notifications_since FROM notification_prefs WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(pool)
            .await?;
    let parsed = since
        .as_deref()
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc));
    if let Some(value) = parsed {
        return Ok((prefs, value));
    }
    sqlx::query("UPDATE notification_prefs SET notifications_since = $1 WHERE user_id = $2")
        .bind(now_utc.to_rfc3339())
        .bind(user_id)
        .execute(pool)
        .await?;
    return Ok((prefs, now_utc));
}

async fn insert_prefs_row(
    pool: &SqlitePool,
    user_id: i64,
    prefs: &NotificationPrefs,
    since: DateTime<Utc>,
    ignore_existing: bool,
) -> Result<(), sqlx::Error> {
    let verb = if ignore_existing {
        "INSERT OR IGNORE"
    } else {
        "INSERT"
    };
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "{verb} INTO notification_prefs (user_id, {PREFS_COLUMNS}, notifications_since) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"
    )))
    .bind(user_id)
    .bind(i64::from(prefs.enabled))
    .bind(i64::from(prefs.day_end_enabled))
    .bind(&prefs.day_end_time)
    .bind(i64::from(prefs.penalty_enabled))
    .bind(&prefs.quiet_start)
    .bind(&prefs.quiet_end)
    .bind(prefs.default_routine_timed_minutes)
    .bind(i64::from(prefs.default_routine_allday_enabled))
    .bind(&prefs.default_routine_allday_time)
    .bind(prefs.default_todo_timed_minutes)
    .bind(prefs.default_todo_allday_days)
    .bind(&prefs.default_todo_allday_time)
    .bind(since.to_rfc3339())
    .execute(pool)
    .await?;
    return Ok(());
}

fn normalize_clock(raw: &str) -> Option<String> {
    let parsed = NaiveTime::parse_from_str(raw.trim(), "%H:%M").ok()?;
    return Some(parsed.format("%H:%M").to_string());
}

fn optional_clock(raw: Option<&str>) -> Result<Option<String>, &'static str> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    return match normalize_clock(raw) {
        Some(value) => Ok(Some(value)),
        None => Err("quiet hours must be HH:MM"),
    };
}

fn checked_minutes(value: Option<i64>) -> Result<Option<i64>, &'static str> {
    return match value {
        Some(minutes) if !(0..=store::max_reminder_offset("minutes")).contains(&minutes) => {
            Err("default minutes must be 0-40320")
        }
        other => Ok(other),
    };
}

pub fn apply_cleared_times(prefs: &mut NotificationPrefs, stored: &NotificationPrefs) {
    if prefs.day_end_time.trim().is_empty() {
        prefs.day_end_enabled = false;
        prefs.day_end_time = stored.day_end_time.clone();
    }
    if prefs.default_routine_allday_time.trim().is_empty() {
        prefs.default_routine_allday_enabled = false;
        prefs.default_routine_allday_time = stored.default_routine_allday_time.clone();
    }
    if prefs.default_todo_allday_time.trim().is_empty() {
        prefs.default_todo_allday_days = None;
        prefs.default_todo_allday_time = stored.default_todo_allday_time.clone();
    }
}

pub fn validate_prefs(prefs: &NotificationPrefs) -> Result<NotificationPrefs, &'static str> {
    let day_end_time =
        normalize_clock(&prefs.day_end_time).ok_or("day end time must be HH:MM")?;
    let quiet_start = optional_clock(prefs.quiet_start.as_deref())?;
    let quiet_end = optional_clock(prefs.quiet_end.as_deref())?;
    if quiet_start.is_some() != quiet_end.is_some() {
        return Err("quiet hours need both a start and an end");
    }
    if quiet_start.is_some() && quiet_start == quiet_end {
        return Err("quiet hours start and end must differ");
    }
    let default_routine_allday_time = normalize_clock(&prefs.default_routine_allday_time)
        .ok_or("default time must be HH:MM")?;
    let default_todo_allday_time =
        normalize_clock(&prefs.default_todo_allday_time).ok_or("default time must be HH:MM")?;
    let default_todo_allday_days = match prefs.default_todo_allday_days {
        Some(days) if !(0..=store::max_reminder_offset("days")).contains(&days) => {
            return Err("default days must be 0-28");
        }
        other => other,
    };
    return Ok(NotificationPrefs {
        enabled: prefs.enabled,
        day_end_enabled: prefs.day_end_enabled,
        day_end_time,
        penalty_enabled: prefs.penalty_enabled,
        quiet_start,
        quiet_end,
        default_routine_timed_minutes: checked_minutes(prefs.default_routine_timed_minutes)?,
        default_routine_allday_enabled: prefs.default_routine_allday_enabled,
        default_routine_allday_time,
        default_todo_timed_minutes: checked_minutes(prefs.default_todo_timed_minutes)?,
        default_todo_allday_days,
        default_todo_allday_time,
    });
}

pub async fn save_prefs(
    pool: &SqlitePool,
    user_id: i64,
    prefs: &NotificationPrefs,
) -> Result<(), sqlx::Error> {
    insert_prefs_row(pool, user_id, &NotificationPrefs::default(), Utc::now(), true).await?;
    sqlx::query(
        "UPDATE notification_prefs SET enabled = $2, day_end_enabled = $3, day_end_time = $4, \
         penalty_enabled = $5, quiet_start = $6, quiet_end = $7, default_routine_timed_minutes = $8, \
         default_routine_allday_enabled = $9, default_routine_allday_time = $10, \
         default_todo_timed_minutes = $11, default_todo_allday_days = $12, \
         default_todo_allday_time = $13 WHERE user_id = $1",
    )
    .bind(user_id)
    .bind(i64::from(prefs.enabled))
    .bind(i64::from(prefs.day_end_enabled))
    .bind(&prefs.day_end_time)
    .bind(i64::from(prefs.penalty_enabled))
    .bind(&prefs.quiet_start)
    .bind(&prefs.quiet_end)
    .bind(prefs.default_routine_timed_minutes)
    .bind(i64::from(prefs.default_routine_allday_enabled))
    .bind(&prefs.default_routine_allday_time)
    .bind(prefs.default_todo_timed_minutes)
    .bind(prefs.default_todo_allday_days)
    .bind(&prefs.default_todo_allday_time)
    .execute(pool)
    .await?;
    return Ok(());
}

pub async fn purge_old(pool: &SqlitePool, now_utc: DateTime<Utc>) -> Result<u64, sqlx::Error> {
    let read_cutoff = (now_utc - Duration::days(READ_RETENTION_DAYS)).to_rfc3339();
    let unread_cutoff = (now_utc - Duration::days(UNREAD_RETENTION_DAYS)).to_rfc3339();
    let result = sqlx::query(
        "DELETE FROM notifications WHERE (read_at IS NOT NULL AND created_at < $1) \
         OR (read_at IS NULL AND created_at < $2)",
    )
    .bind(read_cutoff)
    .bind(unread_cutoff)
    .execute(pool)
    .await?;
    return Ok(result.rows_affected());
}

pub fn spawn(pool: SqlitePool) -> JoinHandle<()> {
    return tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            run_tick(&pool, Utc::now()).await;
        }
    });
}

pub async fn run_tick(pool: &SqlitePool, now_utc: DateTime<Utc>) {
    let users: Vec<(i64, String, String)> =
        match sqlx::query_as("SELECT id, username, timezone FROM users ORDER BY id")
            .fetch_all(pool)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "notification tick could not list users");
                return;
            }
        };
    for (id, username, timezone) in users {
        let user = SessionUser {
            id,
            username,
            timezone,
        };
        if let Err(error) = tick_user(pool, &user, now_utc).await {
            tracing::warn!(user_id = id, %error, "notification tick failed for user");
        }
    }
    let day = now_utc.timestamp().div_euclid(86_400);
    if LAST_PURGE_DAY.swap(day, Ordering::Relaxed) != day
        && let Err(error) = purge_old(pool, now_utc).await
    {
        tracing::warn!(%error, "notification purge failed");
    }
}

async fn tick_user(
    pool: &SqlitePool,
    user: &SessionUser,
    now_utc: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let tz = store::user_timezone(&user.timezone);
    let local_now = now_utc.with_timezone(&tz);
    let today = local_now.date_naive();
    let (prefs, since) = resolve_prefs(pool, user.id, now_utc).await?;
    if let Err(error) = evaluate_if_stale(pool, user, today).await {
        tracing::warn!(user_id = user.id, %error, "penalty evaluation failed");
    }
    if !prefs.enabled || in_quiet_hours(&prefs, local_now.time()) {
        return Ok(());
    }
    routine_reminders(pool, user, tz, now_utc, today).await?;
    todo_reminders(pool, user, tz, now_utc).await?;
    if prefs.day_end_enabled {
        day_end_nudge(pool, user, &prefs, local_now.time(), today, now_utc).await?;
    }
    if prefs.penalty_enabled {
        penalty_events(pool, user, now_utc, since).await?;
    }
    return Ok(());
}

async fn evaluate_if_stale(
    pool: &SqlitePool,
    user: &SessionUser,
    today: NaiveDate,
) -> Result<(), sqlx::Error> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT last_evaluated_day FROM users WHERE id = $1")
            .bind(user.id)
            .fetch_optional(pool)
            .await?;
    let Some((last_day,)) = row else {
        return Ok(());
    };
    let today_str = today.to_string();
    let stale = match last_day {
        Some(day) => day.as_str() < today_str.as_str(),
        None => true,
    };
    if !stale {
        return Ok(());
    }
    punish::evaluate_user(pool, user, today).await?;
    return Ok(());
}

pub fn in_quiet_hours(prefs: &NotificationPrefs, local_time: NaiveTime) -> bool {
    let (Some(start), Some(end)) = (
        prefs.quiet_start.as_deref().and_then(parse_clock),
        prefs.quiet_end.as_deref().and_then(parse_clock),
    ) else {
        return false;
    };
    if start == end {
        return false;
    }
    if start < end {
        return local_time >= start && local_time < end;
    }
    return local_time >= start || local_time < end;
}

fn parse_clock(raw: &str) -> Option<NaiveTime> {
    return NaiveTime::parse_from_str(raw.trim(), "%H:%M").ok();
}

fn local_to_utc(tz: Tz, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    return match tz.from_local_datetime(&date.and_time(time)) {
        LocalResult::Single(value) => Some(value.with_timezone(&Utc)),
        LocalResult::Ambiguous(value, _) => Some(value.with_timezone(&Utc)),
        LocalResult::None => None,
    };
}

fn fire_time(
    tz: Tz,
    event_date: NaiveDate,
    event_time: Option<NaiveTime>,
    offset_value: i64,
    offset_unit: &str,
    at_time: Option<NaiveTime>,
) -> Option<DateTime<Utc>> {
    return match offset_unit {
        "days" | "weeks" => {
            let per_unit = if offset_unit == "weeks" { 7 } else { 1 };
            let days = u64::try_from(offset_value.checked_mul(per_unit)?).ok()?;
            let base = event_date.checked_sub_days(Days::new(days))?;
            local_to_utc(tz, base, at_time?)
        }
        "minutes" | "hours" => {
            let anchor = match event_time {
                Some(time) => local_to_utc(tz, event_date, time)?,
                None => local_to_utc(tz, event_date, at_time?)?,
            };
            let delta = if offset_unit == "hours" {
                Duration::hours(offset_value)
            } else {
                Duration::minutes(offset_value)
            };
            Some(anchor - delta)
        }
        _ => None,
    };
}

fn in_fire_window(
    now_utc: DateTime<Utc>,
    fire: DateTime<Utc>,
    reminder_created_at: &str,
) -> bool {
    let age = now_utc - fire;
    if age < Duration::zero() || age > Duration::minutes(FIRE_WINDOW_MINUTES) {
        return false;
    }
    return match DateTime::parse_from_rfc3339(reminder_created_at) {
        Ok(created) => fire >= created.with_timezone(&Utc),
        Err(_) => true,
    };
}

fn lead_days(offset_value: i64, offset_unit: &str) -> u64 {
    let days = match offset_unit {
        "minutes" => offset_value / 1440 + 1,
        "hours" => offset_value / 24 + 1,
        "weeks" => offset_value.saturating_mul(7),
        _ => offset_value,
    };
    return u64::try_from(days).unwrap_or(0);
}

fn plural(value: i64, unit: &str) -> String {
    if value == 1 {
        return format!("1 {}", unit.trim_end_matches('s'));
    }
    return format!("{value} {unit}");
}

fn reminder_body(
    is_todo: bool,
    offset_value: i64,
    offset_unit: &str,
    event_date: NaiveDate,
    fire_date: NaiveDate,
    event_time: Option<&str>,
) -> String {
    if matches!(offset_unit, "minutes" | "hours") && event_time.is_some() {
        let lead = if is_todo { "Deadline" } else { "Starts" };
        if offset_value == 0 {
            return format!("{lead} now");
        }
        return format!("{lead} in {}", plural(offset_value, offset_unit));
    }
    let days = (event_date - fire_date).num_days();
    let when = match days {
        0 => "today".to_string(),
        1 => "tomorrow".to_string(),
        _ if offset_unit == "weeks" => format!("in {}", plural(offset_value, "weeks")),
        _ => format!("in {days} days"),
    };
    let suffix = event_time
        .map(|time| format!(" at {time}"))
        .unwrap_or_default();
    if is_todo {
        return format!("Deadline {when}{suffix}");
    }
    let mut chars = when.chars();
    let head = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
        None => String::new(),
    };
    return format!("{head}{suffix}");
}

#[allow(clippy::too_many_arguments)]
async fn insert_notification(
    pool: &SqlitePool,
    user_id: i64,
    kind: &str,
    title: &str,
    body: Option<&str>,
    url: Option<&str>,
    dedupe_key: &str,
    now_utc: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "INSERT OR IGNORE INTO notifications (user_id, kind, title, body, url, dedupe_key, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(user_id)
    .bind(kind)
    .bind(title)
    .bind(body)
    .bind(url)
    .bind(dedupe_key)
    .bind(now_utc.to_rfc3339())
    .execute(pool)
    .await?;
    return Ok(result.rows_affected() > 0);
}

fn candidate_dates(schedule: &Schedule, lo: NaiveDate, hi: NaiveDate) -> Vec<NaiveDate> {
    if schedule.kind == ScheduleKind::Weekdays {
        return occurrences_in_range(schedule, lo, hi);
    }
    let mut dates = Vec::new();
    let mut current = lo;
    while current <= hi {
        if !periods_in_range(schedule, current, current).is_empty() {
            dates.push(current);
        }
        match current.checked_add_days(Days::new(1)) {
            Some(next) => current = next,
            None => break,
        }
    }
    return dates;
}

async fn occurrence_completed(
    pool: &SqlitePool,
    routine_id: i64,
    date: NaiveDate,
) -> Result<bool, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM completions c JOIN occurrences o ON o.id = c.occurrence_id \
         WHERE o.routine_id = $1 AND o.due_date = $2",
    )
    .bind(routine_id)
    .bind(date.to_string())
    .fetch_one(pool)
    .await?;
    return Ok(count > 0);
}

async fn period_goal_met(
    pool: &SqlitePool,
    routine_id: i64,
    schedule: &Schedule,
    date: NaiveDate,
) -> Result<bool, sqlx::Error> {
    let Some((start, _)) = period_bounds(date, schedule.period) else {
        return Ok(false);
    };
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM completions c JOIN occurrences o ON o.id = c.occurrence_id \
         WHERE o.routine_id = $1 AND o.period_start = $2",
    )
    .bind(routine_id)
    .bind(start.to_string())
    .fetch_one(pool)
    .await?;
    return Ok(count >= i64::from(schedule.per_period));
}

async fn routine_reminders(
    pool: &SqlitePool,
    user: &SessionUser,
    tz: Tz,
    now_utc: DateTime<Utc>,
    today: NaiveDate,
) -> Result<(), sqlx::Error> {
    let reminders: Vec<ReminderTickRecord> = sqlx::query_as(
        "SELECT id, target_id, created_at, offset_value, offset_unit, at_time FROM reminders \
         WHERE user_id = $1 AND target_type = 'routine' ORDER BY id",
    )
    .bind(user.id)
    .fetch_all(pool)
    .await?;
    let mut routines: HashMap<i64, Option<store::RoutineRow>> = HashMap::new();
    for (reminder_id, routine_id, created_at, offset_value, offset_unit, at_time) in reminders {
        if let Entry::Vacant(slot) = routines.entry(routine_id) {
            slot.insert(store::routine_for_user(pool, user.id, routine_id).await?);
        }
        let Some(Some(routine)) = routines.get(&routine_id) else {
            continue;
        };
        if routine.archived_at.is_some() {
            continue;
        }
        let Some(schedule) = store::schedule_from_row(routine) else {
            continue;
        };
        let event_time = routine.time_of_day.as_deref().and_then(parse_clock);
        let at_time = at_time.as_deref().and_then(parse_clock);
        let Some(lo) = today.checked_sub_days(Days::new(1)) else {
            continue;
        };
        let Some(hi) = today.checked_add_days(Days::new(
            lead_days(offset_value, &offset_unit).saturating_add(2),
        )) else {
            continue;
        };
        for date in candidate_dates(&schedule, lo, hi) {
            let Some(fire) = fire_time(tz, date, event_time, offset_value, &offset_unit, at_time)
            else {
                continue;
            };
            if !in_fire_window(now_utc, fire, &created_at) {
                continue;
            }
            if occurrence_completed(pool, routine.id, date).await? {
                continue;
            }
            if schedule.kind == ScheduleKind::Count
                && period_goal_met(pool, routine.id, &schedule, date).await?
            {
                continue;
            }
            let label = event_time.map(|time| time.format("%H:%M").to_string());
            let body = reminder_body(
                false,
                offset_value,
                &offset_unit,
                date,
                fire.with_timezone(&tz).date_naive(),
                label.as_deref(),
            );
            insert_notification(
                pool,
                user.id,
                "routine_reminder",
                &routine.title,
                Some(&body),
                Some(&format!("/day?date={date}")),
                &format!("rem:{reminder_id}:{date}"),
                now_utc,
            )
            .await?;
        }
    }
    return Ok(());
}

async fn todo_reminders(
    pool: &SqlitePool,
    user: &SessionUser,
    tz: Tz,
    now_utc: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let rows: Vec<TodoReminderRecord> = sqlx::query_as(
        "SELECT rem.id, rem.created_at, rem.offset_value, rem.offset_unit, rem.at_time, \
         t.id, t.title, t.deadline, t.all_day \
         FROM reminders rem JOIN todos t ON t.id = rem.target_id \
         WHERE rem.user_id = $1 AND rem.target_type = 'todo' \
         AND t.completed_at IS NULL AND t.deadline IS NOT NULL ORDER BY rem.id",
    )
    .bind(user.id)
    .fetch_all(pool)
    .await?;
    for (reminder_id, created_at, offset_value, offset_unit, at_time, _, title, deadline, all_day) in
        rows
    {
        let Ok(deadline) = DateTime::parse_from_rfc3339(&deadline) else {
            continue;
        };
        let local = deadline.with_timezone(&tz);
        let date = local.date_naive();
        let event_time = if all_day == 0 {
            NaiveTime::from_hms_opt(
                chrono::Timelike::hour(&local),
                chrono::Timelike::minute(&local),
                0,
            )
        } else {
            None
        };
        let at_time = at_time.as_deref().and_then(parse_clock);
        let Some(fire) = fire_time(tz, date, event_time, offset_value, &offset_unit, at_time)
        else {
            continue;
        };
        if !in_fire_window(now_utc, fire, &created_at) {
            continue;
        }
        let label = event_time.map(|time| time.format("%H:%M").to_string());
        let body = reminder_body(
            true,
            offset_value,
            &offset_unit,
            date,
            fire.with_timezone(&tz).date_naive(),
            label.as_deref(),
        );
        insert_notification(
            pool,
            user.id,
            "todo_reminder",
            &title,
            Some(&body),
            Some(&format!("/day?date={date}")),
            &format!("rem:{reminder_id}:{date}"),
            now_utc,
        )
        .await?;
    }
    return Ok(());
}

async fn day_end_nudge(
    pool: &SqlitePool,
    user: &SessionUser,
    prefs: &NotificationPrefs,
    local_time: NaiveTime,
    today: NaiveDate,
    now_utc: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let Some(threshold) = parse_clock(&prefs.day_end_time) else {
        return Ok(());
    };
    if local_time < threshold {
        return Ok(());
    }
    let key = format!("dayend:{today}");
    let existing: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM notifications WHERE user_id = $1 AND dedupe_key = $2")
            .bind(user.id)
            .bind(&key)
            .fetch_optional(pool)
            .await?;
    if existing.is_some() {
        return Ok(());
    }
    let routines = store::day_routines(pool, user.id, today, today).await?;
    let open: Vec<&str> = routines
        .iter()
        .filter(|info| {
            !info.completed
                && match (info.period_done, info.period_required) {
                    (Some(done), Some(required)) => done < required,
                    _ => true,
                }
        })
        .map(|info| info.routine.title.as_str())
        .collect();
    if open.is_empty() {
        return Ok(());
    }
    let title = if open.len() == 1 {
        "1 routine still open today".to_string()
    } else {
        format!("{} routines still open today", open.len())
    };
    let mut body = open
        .iter()
        .take(DAY_END_LIST_CAP)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if open.len() > DAY_END_LIST_CAP {
        body.push_str(&format!(" and {} more", open.len() - DAY_END_LIST_CAP));
    }
    insert_notification(
        pool,
        user.id,
        "day_end",
        &title,
        Some(&body),
        Some(&format!("/day?date={today}")),
        &key,
        now_utc,
    )
    .await?;
    return Ok(());
}

async fn penalty_events(
    pool: &SqlitePool,
    user: &SessionUser,
    now_utc: DateTime<Utc>,
    since: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let cutoff = (now_utc - Duration::hours(PENALTY_LOOKBACK_HOURS))
        .max(since)
        .to_rfc3339();
    let issued: Vec<(i64, String)> = sqlx::query_as(
        "SELECT p.id, COALESCE(c.title, p.custom_title) FROM punishments p \
         LEFT JOIN punishment_catalog c ON c.id = p.catalog_id \
         WHERE p.user_id = $1 AND p.assigned_at >= $2 \
         AND COALESCE(c.title, p.custom_title) IS NOT NULL ORDER BY p.id",
    )
    .bind(user.id)
    .bind(&cutoff)
    .fetch_all(pool)
    .await?;
    for (id, title) in issued {
        insert_notification(
            pool,
            user.id,
            "penalty_issued",
            "Penalty issued",
            Some(&title),
            Some("/punishments"),
            &format!("pun:{id}:issued"),
            now_utc,
        )
        .await?;
    }
    let resolved: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT p.id, p.status, COALESCE(c.title, p.custom_title) FROM punishments p \
         LEFT JOIN punishment_catalog c ON c.id = p.catalog_id \
         WHERE p.user_id = $1 AND p.status IN ('cleared', 'waived') AND p.cleared_at >= $2 \
         AND COALESCE(c.title, p.custom_title) IS NOT NULL ORDER BY p.id",
    )
    .bind(user.id)
    .bind(&cutoff)
    .fetch_all(pool)
    .await?;
    for (id, status, title) in resolved {
        let heading = if status == "waived" {
            "Penalty waived"
        } else {
            "Penalty cleared"
        };
        insert_notification(
            pool,
            user.id,
            "penalty_resolved",
            heading,
            Some(&title),
            Some("/punishments"),
            &format!("pun:{id}:{status}"),
            now_utc,
        )
        .await?;
    }
    return Ok(());
}
