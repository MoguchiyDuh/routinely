use std::cmp::Ordering;
use std::collections::HashMap;
use std::collections::HashSet;

use chrono::Datelike;
use chrono::Days;
use chrono::NaiveDate;
use chrono::Weekday;
use sqlx::SqlitePool;

use crate::recurrence::PeriodKind;
use crate::recurrence::Schedule;
use crate::recurrence::ScheduleKind;
use crate::recurrence::effective_end;
use crate::recurrence::mask_contains;
use crate::recurrence::occurrences_in_range;
use crate::recurrence::periods_in_range;
use crate::store;

pub const WINDOW_7_DAYS: u64 = 7;
pub const WINDOW_30_DAYS: u64 = 30;
pub const STREAK_SCAN_DAYS: u64 = 365;
pub const STREAK_CAP_DAYS: u64 = 3650;
pub const HEATMAP_WEEKS: u64 = 12;

const WEEKDAYS: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];

const WEEKDAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

#[derive(Debug, Clone, Copy)]
pub struct UserStats {
    pub pct_7d: Option<f64>,
    pub pct_30d: Option<f64>,
    pub streak: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct LeaderRow {
    pub user_id: i64,
    pub username: String,
    pub pct_7d: Option<f64>,
    pub pct_30d: Option<f64>,
    pub streak: Option<u64>,
    pub pending: u64,
}

#[derive(Debug, Clone)]
pub struct HeatDay {
    pub date: NaiveDate,
    pub done: u32,
    pub expected: u32,
    pub level: u8,
    pub future: bool,
}

pub fn window_start(today: NaiveDate, days: u64) -> Option<NaiveDate> {
    if days == 0 {
        return Some(today);
    }
    return today.checked_sub_days(Days::new(days - 1));
}

pub async fn stats_for_user(
    pool: &SqlitePool,
    user_id: i64,
    timezone: &str,
    routines: &[store::RoutineRow],
) -> Result<UserStats, sqlx::Error> {
    let today = store::user_today(timezone);
    let horizon = streak_horizon(today);
    let satisfied = satisfied_weekdays(pool, user_id, horizon, today).await?;
    let counts = period_counts(pool, user_id).await?;
    let pct_7d = match window_start(today, WINDOW_7_DAYS) {
        Some(start) => window_pct(routines, &satisfied, &counts, start, today),
        None => None,
    };
    let pct_30d = match window_start(today, WINDOW_30_DAYS) {
        Some(start) => window_pct(routines, &satisfied, &counts, start, today),
        None => None,
    };
    let streak = compute_streak(routines, &satisfied, &counts, today, horizon);
    return Ok(UserStats {
        pct_7d,
        pct_30d,
        streak,
    });
}

pub async fn routine_window_pcts(
    pool: &SqlitePool,
    user_id: i64,
    timezone: &str,
    routines: &[store::RoutineRow],
) -> Result<Vec<Option<f64>>, sqlx::Error> {
    let today = store::user_today(timezone);
    let Some(start) = window_start(today, WINDOW_30_DAYS) else {
        return Ok(routines.iter().map(|_| None).collect());
    };
    let satisfied = satisfied_weekdays(pool, user_id, start, today).await?;
    let counts = period_counts(pool, user_id).await?;
    let mut out = Vec::new();
    for routine in routines {
        out.push(window_pct(
            std::slice::from_ref(routine),
            &satisfied,
            &counts,
            start,
            today,
        ));
    }
    return Ok(out);
}

pub async fn leaderboard_rows(pool: &SqlitePool) -> Result<Vec<LeaderRow>, sqlx::Error> {
    let users: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT id, username, timezone FROM users ORDER BY id")
            .fetch_all(pool)
            .await?;
    let mut routines_by_user: HashMap<i64, Vec<store::RoutineRow>> = HashMap::new();
    for holder in store::active_routines_with_users(pool).await? {
        routines_by_user
            .entry(holder.routine.user_id)
            .or_default()
            .push(holder.routine);
    }
    let pending = pending_counts(pool).await?;
    let mut rows = Vec::new();
    for (user_id, username, timezone) in users {
        let empty: Vec<store::RoutineRow> = Vec::new();
        let routine_list = routines_by_user.get(&user_id).unwrap_or(&empty);
        let stats = stats_for_user(pool, user_id, &timezone, routine_list).await?;
        rows.push(LeaderRow {
            user_id,
            username,
            pct_7d: stats.pct_7d,
            pct_30d: stats.pct_30d,
            streak: stats.streak,
            pending: pending.get(&user_id).copied().unwrap_or(0),
        });
    }
    sort_rows(&mut rows);
    return Ok(rows);
}

pub fn sort_rows(rows: &mut [LeaderRow]) {
    rows.sort_by(compare_rows);
}

pub fn describe_schedule(
    schedule: &Schedule,
    time_of_day: Option<&str>,
    time_end: Option<&str>,
    today: NaiveDate,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(schedule_head(schedule));
    if schedule.start_date > today {
        parts.push(format!("starts {}", schedule.start_date.format("%a %-d %b")));
    }
    parts.push(schedule_bounds(schedule, today));
    if let Some(time) = time_window(time_of_day, time_end) {
        parts.push(time);
    }
    return parts.join(" \u{00b7} ");
}

fn compare_rows(a: &LeaderRow, b: &LeaderRow) -> Ordering {
    let pct = compare_pct(a.pct_7d, b.pct_7d);
    if pct != Ordering::Equal {
        return pct;
    }
    let streak = compare_streak(a.streak, b.streak);
    if streak != Ordering::Equal {
        return streak;
    }
    return a.username.cmp(&b.username);
}

fn compare_pct(a: Option<f64>, b: Option<f64>) -> Ordering {
    return match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(Ordering::Equal),
    };
}

fn compare_streak(a: Option<u64>, b: Option<u64>) -> Ordering {
    return match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => y.cmp(&x),
    };
}

async fn pending_counts(pool: &SqlitePool) -> Result<HashMap<i64, u64>, sqlx::Error> {
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT user_id, COUNT(*) FROM punishments WHERE status = 'pending' GROUP BY user_id",
    )
    .fetch_all(pool)
    .await?;
    let mut out = HashMap::new();
    for (user_id, count) in rows {
        out.insert(user_id, u64::try_from(count).unwrap_or(0));
    }
    return Ok(out);
}

async fn satisfied_weekdays(
    pool: &SqlitePool,
    user_id: i64,
    since: NaiveDate,
    until_exclusive: NaiveDate,
) -> Result<HashSet<(i64, NaiveDate)>, sqlx::Error> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT o.routine_id, o.due_date \
         FROM completions c \
         JOIN occurrences o ON o.id = c.occurrence_id \
         JOIN routines r ON r.id = o.routine_id \
         WHERE r.user_id = $1 AND r.archived_at IS NULL \
           AND o.period_start IS NULL AND o.due_date >= $2 AND o.due_date < $3",
    )
    .bind(user_id)
    .bind(since.to_string())
    .bind(until_exclusive.to_string())
    .fetch_all(pool)
    .await?;
    let mut out = HashSet::new();
    for (routine_id, due_date) in rows {
        let Ok(date) = NaiveDate::parse_from_str(&due_date, "%Y-%m-%d") else {
            continue;
        };
        out.insert((routine_id, date));
    }
    return Ok(out);
}

async fn period_counts(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<HashMap<(i64, String), u32>, sqlx::Error> {
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT o.routine_id, o.period_start, COUNT(*) \
         FROM completions c \
         JOIN occurrences o ON o.id = c.occurrence_id \
         JOIN routines r ON r.id = o.routine_id \
         WHERE r.user_id = $1 AND r.archived_at IS NULL AND o.period_start IS NOT NULL \
         GROUP BY o.routine_id, o.period_start",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let mut out = HashMap::new();
    for (routine_id, period_start, count) in rows {
        out.insert(
            (routine_id, period_start),
            u32::try_from(count).unwrap_or(0),
        );
    }
    return Ok(out);
}

fn window_pct(
    routines: &[store::RoutineRow],
    satisfied: &HashSet<(i64, NaiveDate)>,
    counts: &HashMap<(i64, String), u32>,
    window_start: NaiveDate,
    today: NaiveDate,
) -> Option<f64> {
    let mut due = 0u64;
    let mut total_satisfied = 0u64;
    for routine in routines {
        let Some(schedule) = store::schedule_from_row(routine) else {
            continue;
        };
        match schedule.kind {
            ScheduleKind::Weekdays => {
                let Some(closed_end) = today.pred_opt() else {
                    continue;
                };
                for date in occurrences_in_range(&schedule, window_start, closed_end) {
                    due += 1;
                    if satisfied.contains(&(routine.id, date)) {
                        total_satisfied += 1;
                    }
                }
            }
            ScheduleKind::Count => {
                for period in periods_in_range(&schedule, window_start, today) {
                    if period.period_end >= today || period.period_end < window_start {
                        continue;
                    }
                    due += 1;
                    let done = counts
                        .get(&(routine.id, period.period_start.to_string()))
                        .copied()
                        .unwrap_or(0);
                    if done >= period.required {
                        total_satisfied += 1;
                    }
                }
            }
        }
    }
    if due == 0 {
        return None;
    }
    return Some(total_satisfied as f64 / due as f64);
}

fn compute_streak(
    routines: &[store::RoutineRow],
    satisfied: &HashSet<(i64, NaiveDate)>,
    counts: &HashMap<(i64, String), u32>,
    today: NaiveDate,
    horizon: NaiveDate,
) -> Option<u64> {
    if routines.is_empty() {
        return None;
    }
    let mut latest_miss: Option<NaiveDate> = None;
    for routine in routines {
        let Some(schedule) = store::schedule_from_row(routine) else {
            continue;
        };
        match schedule.kind {
            ScheduleKind::Weekdays => {
                let Some(closed_end) = today.pred_opt() else {
                    continue;
                };
                for date in occurrences_in_range(&schedule, horizon, closed_end) {
                    if satisfied.contains(&(routine.id, date)) {
                        continue;
                    }
                    latest_miss = Some(match latest_miss {
                        Some(current) if current > date => current,
                        _ => date,
                    });
                }
            }
            ScheduleKind::Count => {
                for period in periods_in_range(&schedule, horizon, today) {
                    if period.period_end >= today {
                        continue;
                    }
                    let done = counts
                        .get(&(routine.id, period.period_start.to_string()))
                        .copied()
                        .unwrap_or(0);
                    if done >= period.required {
                        continue;
                    }
                    latest_miss = Some(match latest_miss {
                        Some(current) if current > period.period_end => current,
                        _ => period.period_end,
                    });
                }
            }
        }
    }
    if let Some(miss) = latest_miss {
        return Some(u64::try_from((today - miss).num_days()).unwrap_or(0));
    }
    let oldest = routines
        .iter()
        .filter_map(|routine| routine.start_date)
        .min()?;
    return Some(
        u64::try_from((today - oldest).num_days())
            .unwrap_or(0)
            .min(STREAK_CAP_DAYS),
    );
}

fn streak_horizon(today: NaiveDate) -> NaiveDate {
    return today
        .checked_sub_days(Days::new(STREAK_SCAN_DAYS))
        .unwrap_or(NaiveDate::MIN);
}

fn schedule_head(schedule: &Schedule) -> String {
    return match schedule.kind {
        ScheduleKind::Weekdays => {
            let mut names: Vec<&str> = Vec::new();
            for (index, weekday) in WEEKDAYS.iter().enumerate() {
                if mask_contains(schedule.weekdays_mask, *weekday) {
                    names.push(WEEKDAY_NAMES[index]);
                }
            }
            if names.is_empty() {
                return "no days".to_string();
            }
            return names.join("/");
        }
        ScheduleKind::Count => {
            format!(
                "{}\u{00d7}/{}",
                schedule.per_period,
                period_name(schedule.period)
            )
        }
    };
}

fn schedule_bounds(schedule: &Schedule, today: NaiveDate) -> String {
    if let Some(end) = schedule.end_date {
        return format!("until {end}");
    }
    return match schedule.kind {
        ScheduleKind::Weekdays => match schedule.iterations {
            Some(weeks) => format!("{weeks} weeks"),
            None => "infinite".to_string(),
        },
        ScheduleKind::Count => match schedule.iterations {
            Some(total) => {
                let started = periods_in_range(schedule, schedule.start_date, today).len();
                let remaining = total.saturating_sub(u32::try_from(started).unwrap_or(u32::MAX));
                return format!("{remaining} more periods");
            }
            None => "infinite".to_string(),
        },
    };
}

fn time_window(time_of_day: Option<&str>, time_end: Option<&str>) -> Option<String> {
    let start = time_of_day?;
    return match time_end {
        Some(end) => Some(format!("{start}\u{2013}{end}")),
        None => Some(start.to_string()),
    };
}

fn period_name(period: PeriodKind) -> &'static str {
    return match period {
        PeriodKind::Day => "day",
        PeriodKind::Week => "week",
        PeriodKind::Month => "month",
        PeriodKind::Year => "year",
    };
}

pub fn heatmap_level(done: u32, expected: u32) -> u8 {
    if expected == 0 {
        return 0;
    }
    if done >= expected {
        return 4;
    }
    let ratio = f64::from(done) / f64::from(expected);
    if ratio < 0.25 {
        return 1;
    }
    if ratio < 0.5 {
        return 2;
    }
    return 3;
}

pub async fn heatmap(
    pool: &SqlitePool,
    user_id: i64,
    timezone: &str,
) -> Result<Vec<HeatDay>, sqlx::Error> {
    let today = store::user_today(timezone);
    let routines = store::all_routines(pool, user_id).await?;
    let completions = completions_by_day(pool, user_id).await?;
    let start = match week_start_of(today).checked_sub_days(Days::new((HEATMAP_WEEKS - 1) * 7)) {
        Some(start) => start,
        None => return Ok(Vec::new()),
    };
    let total_days = HEATMAP_WEEKS * 7;
    let mut days = Vec::new();
    for offset in 0..total_days {
        let Some(date) = start.checked_add_days(Days::new(offset)) else {
            continue;
        };
        let future = date > today;
        let mut done = 0u32;
        let mut expected = 0u32;
        if !future {
            for routine in &routines {
                match routine_coverage(routine, date) {
                    RoutineCoverage::Scheduled => {
                        expected += 1;
                        if completions.get(&(routine.id, date)).copied().unwrap_or(0) > 0 {
                            done += 1;
                        }
                    }
                    RoutineCoverage::PerCompletion => {
                        let count = completions.get(&(routine.id, date)).copied().unwrap_or(0);
                        done += count;
                        expected += count;
                    }
                    RoutineCoverage::None => {}
                }
            }
        }
        days.push(HeatDay {
            date,
            done,
            expected,
            level: heatmap_level(done, expected),
            future,
        });
    }
    return Ok(days);
}

enum RoutineCoverage {
    None,
    Scheduled,
    PerCompletion,
}

fn routine_coverage(routine: &store::RoutineRow, date: NaiveDate) -> RoutineCoverage {
    let Some(schedule) = store::schedule_from_row(routine) else {
        return RoutineCoverage::None;
    };
    if date < schedule.start_date {
        return RoutineCoverage::None;
    }
    if let Some(end) = effective_end(&schedule)
        && date > end
    {
        return RoutineCoverage::None;
    }
    return match schedule.kind {
        ScheduleKind::Weekdays => {
            if mask_contains(schedule.weekdays_mask, date.weekday()) {
                RoutineCoverage::Scheduled
            } else {
                RoutineCoverage::None
            }
        }
        ScheduleKind::Count => RoutineCoverage::PerCompletion,
    };
}

async fn completions_by_day(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<HashMap<(i64, NaiveDate), u32>, sqlx::Error> {
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT o.routine_id, o.due_date, COUNT(*) \
         FROM completions c \
         JOIN occurrences o ON o.id = c.occurrence_id \
         JOIN routines r ON r.id = o.routine_id \
         WHERE r.user_id = $1 \
         GROUP BY o.routine_id, o.due_date",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let mut out = HashMap::new();
    for (routine_id, due_date, count) in rows {
        let Ok(date) = NaiveDate::parse_from_str(&due_date, "%Y-%m-%d") else {
            continue;
        };
        out.insert((routine_id, date), u32::try_from(count).unwrap_or(0));
    }
    return Ok(out);
}

fn week_start_of(date: NaiveDate) -> NaiveDate {
    let offset = u64::from(date.weekday().num_days_from_monday());
    return date.checked_sub_days(Days::new(offset)).unwrap_or(date);
}
