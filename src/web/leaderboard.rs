use std::collections::HashMap;

use askama::Template;
use axum::Router;
use axum::extract::Path;
use axum::extract::State;
use axum::routing::get;
use chrono::NaiveDate;
use serde::Serialize;
use sqlx::SqlitePool;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::recurrence::PeriodKind;
use crate::score;
use crate::store;

use super::WebError;
use super::WebResult;
use super::html;
use super::shell::Shell;

#[derive(Template)]
#[template(path = "leaderboard.html")]
pub struct LeaderboardPage {
    pub shell: Shell,
    rows: Vec<LeaderRowVm>,
}

struct LeaderRowVm {
    rank: u32,
    user_id: i64,
    username: String,
    pct_7d: String,
    pct_30d: String,
    streak: String,
    pending: u64,
}

#[derive(Template)]
#[template(path = "profile.html")]
pub struct ProfilePage {
    pub shell: Shell,
    username: String,
    timezone: String,
    member_since: String,
    pct_7d: String,
    pct_30d: String,
    streak: String,
    punishments_pending: u64,
    punishments_cleared: u64,
    punishments_waived: u64,
    routines: Vec<ProfileRoutineJson>,
    pins: Vec<ProfilePinJson>,
}

#[derive(Serialize)]
struct LeaderboardJsonRow {
    rank: u32,
    user_id: i64,
    username: String,
    pct_7d: Option<f64>,
    pct_30d: Option<f64>,
    streak_days: Option<u64>,
    pending_punishments: u64,
}

#[derive(Serialize)]
struct ProfileJson {
    id: i64,
    username: String,
    timezone: String,
    member_since: String,
    pct_7d: Option<f64>,
    pct_30d: Option<f64>,
    streak_days: Option<u64>,
    punishments: PunishmentCountsJson,
    routines: Vec<ProfileRoutineJson>,
    pins: Vec<ProfilePinJson>,
}

#[derive(Serialize)]
struct PunishmentCountsJson {
    pending: u64,
    cleared: u64,
    waived: u64,
}

#[derive(Serialize)]
struct ProfileRoutineJson {
    id: i64,
    title: String,
    description: String,
    schedule: ScheduleJson,
}

#[derive(Serialize)]
struct ScheduleJson {
    kind: String,
    weekdays_mask: Option<u8>,
    period: Option<String>,
    per_period: Option<u32>,
    iterations: Option<u32>,
    start_date: Option<NaiveDate>,
    end_date: Option<NaiveDate>,
}

#[derive(Serialize)]
struct ProfilePinJson {
    occurrence_id: i64,
    due_date: NaiveDate,
    summary: String,
    links: Vec<String>,
    photos: Vec<String>,
}

struct ProfileData {
    user_id: i64,
    username: String,
    timezone: String,
    member_since: String,
    stats: score::UserStats,
    punishments: PunishmentCountsJson,
    routines: Vec<ProfileRoutineJson>,
    pins: Vec<ProfilePinJson>,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/leaderboard", get(leaderboard_view))
        .route("/api/v1/leaderboard", get(api_leaderboard))
        .route("/users/{id}", get(profile_view))
        .route("/api/v1/users/{id}", get(api_profile));
}

async fn leaderboard_view(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> WebResult {
    let shell = Shell::load(&state, &auth, "group", &headers).await?;
    let rows = score::leaderboard_rows(&state.pool).await?;
    let vms = rows
        .iter()
        .enumerate()
        .map(|(index, row)| LeaderRowVm {
            rank: u32::try_from(index + 1).unwrap_or(0),
            user_id: row.user_id,
            username: row.username.clone(),
            pct_7d: pct_label(row.pct_7d),
            pct_30d: pct_label(row.pct_30d),
            streak: streak_label(row.streak),
            pending: row.pending,
        })
        .collect();
    let page = LeaderboardPage { shell, rows: vms };
    return Ok(html(&page)?);
}

async fn api_leaderboard(
    ApiAuth(_auth): ApiAuth,
    State(state): State<AppState>,
) -> Result<axum::Json<Vec<LeaderboardJsonRow>>, WebError> {
    let rows = score::leaderboard_rows(&state.pool).await?;
    let payload = rows
        .iter()
        .enumerate()
        .map(|(index, row)| LeaderboardJsonRow {
            rank: u32::try_from(index + 1).unwrap_or(0),
            user_id: row.user_id,
            username: row.username.clone(),
            pct_7d: row.pct_7d,
            pct_30d: row.pct_30d,
            streak_days: row.streak,
            pending_punishments: row.pending,
        })
        .collect();
    return Ok(axum::Json(payload));
}

async fn profile_view(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(user_id): Path<i64>,
    headers: axum::http::HeaderMap,
) -> WebResult {
    let Some(data) = build_profile(&state.pool, user_id).await? else {
        return Err(WebError::NotFound);
    };
    let shell = Shell::load(&state, &auth, "group", &headers).await?;
    let page = ProfilePage {
        shell,
        username: data.username,
        timezone: data.timezone,
        member_since: data.member_since,
        pct_7d: pct_label(data.stats.pct_7d),
        pct_30d: pct_label(data.stats.pct_30d),
        streak: streak_label(data.stats.streak),
        punishments_pending: data.punishments.pending,
        punishments_cleared: data.punishments.cleared,
        punishments_waived: data.punishments.waived,
        routines: data.routines,
        pins: data.pins,
    };
    return Ok(html(&page)?);
}

async fn api_profile(
    ApiAuth(_auth): ApiAuth,
    State(state): State<AppState>,
    Path(user_id): Path<i64>,
) -> Result<axum::Json<ProfileJson>, WebError> {
    let Some(data) = build_profile(&state.pool, user_id).await? else {
        return Err(WebError::NotFound);
    };
    return Ok(axum::Json(ProfileJson {
        id: data.user_id,
        username: data.username,
        timezone: data.timezone,
        member_since: data.member_since,
        pct_7d: data.stats.pct_7d,
        pct_30d: data.stats.pct_30d,
        streak_days: data.stats.streak,
        punishments: data.punishments,
        routines: data.routines,
        pins: data.pins,
    }));
}

async fn build_profile(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<Option<ProfileData>, sqlx::Error> {
    let row: Option<(i64, String, String, String)> =
        sqlx::query_as("SELECT id, username, timezone, created_at FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    let Some((id, username, timezone, created_at)) = row else {
        return Ok(None);
    };
    let mut conn = pool.acquire().await?;
    let routines = store::active_routines(&mut conn, id).await?;
    drop(conn);
    let stats = score::stats_for_user(pool, id, &timezone, &routines).await?;
    let punishments = punishment_counts(pool, id).await?;
    let pins = recent_pins(pool, id).await?;
    let today = store::user_today(&timezone);
    let child_ids = store::chain_child_ids(pool, id).await?;
    let mut routine_payloads = Vec::new();
    for routine in routines.iter().filter(|r| !child_ids.contains(&r.id)) {
        let description = match store::schedule_from_row(routine) {
            Some(schedule) => score::describe_schedule(
                &schedule,
                routine.time_of_day.as_deref(),
                routine.time_end.as_deref(),
                today,
            ),
            None => String::new(),
        };
        routine_payloads.push(ProfileRoutineJson {
            id: routine.id,
            title: routine.title.clone(),
            description,
            schedule: schedule_json(routine),
        });
    }
    let member_since = created_at.get(..10).unwrap_or(&created_at).to_string();
    return Ok(Some(ProfileData {
        user_id: id,
        username,
        timezone,
        member_since,
        stats,
        punishments,
        routines: routine_payloads,
        pins,
    }));
}

fn pct_label(pct: Option<f64>) -> String {
    return match pct {
        Some(value) => format!("{}%", (value * 100.0).round() as i64),
        None => "\u{2014}".to_string(),
    };
}

fn streak_label(streak: Option<u64>) -> String {
    return match streak {
        Some(days) => days.to_string(),
        None => "\u{2014}".to_string(),
    };
}

fn schedule_json(routine: &store::RoutineRow) -> ScheduleJson {
    let period = routine.period.map(|value| match value {
        PeriodKind::Day => "day",
        PeriodKind::Week => "week",
        PeriodKind::Month => "month",
        PeriodKind::Year => "year",
    });
    let kind = if routine.kind.as_str() == "count" {
        "count"
    } else {
        "weekdays"
    };
    return ScheduleJson {
        kind: kind.to_string(),
        weekdays_mask: routine.weekdays_mask,
        period: period.map(str::to_string),
        per_period: routine.per_period,
        iterations: routine.iterations,
        start_date: routine.start_date,
        end_date: routine.end_date,
    };
}

async fn punishment_counts(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<PunishmentCountsJson, sqlx::Error> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*) FROM punishments WHERE user_id = ? GROUP BY status",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let mut counts = PunishmentCountsJson {
        pending: 0,
        cleared: 0,
        waived: 0,
    };
    for (status, count) in rows {
        let value = u64::try_from(count).unwrap_or(0);
        match status.as_str() {
            "pending" => counts.pending = value,
            "cleared" => counts.cleared = value,
            "waived" => counts.waived = value,
            _ => {}
        }
    }
    return Ok(counts);
}

async fn recent_pins(pool: &SqlitePool, user_id: i64) -> Result<Vec<ProfilePinJson>, sqlx::Error> {
    let rows: Vec<(i64, i64, String, String)> = sqlx::query_as(
        "SELECT p.id, o.id, o.due_date, p.summary \
         FROM pins p \
         JOIN completions c ON c.id = p.completion_id \
         JOIN occurrences o ON o.id = c.occurrence_id \
         JOIN routines r ON r.id = o.routine_id \
         WHERE r.user_id = ? \
         ORDER BY p.created_at DESC, p.id DESC \
         LIMIT 10",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let pin_ids: Vec<i64> = rows.iter().map(|(pin_id, _, _, _)| *pin_id).collect();
    let mut attachments: HashMap<i64, (Vec<String>, Vec<String>)> = HashMap::new();
    if !pin_ids.is_empty() {
        let mut builder = sqlx::QueryBuilder::new(
            "SELECT pin_id, kind, url, file_path FROM attachments WHERE pin_id IN (",
        );
        let mut separated = builder.separated(", ");
        for pin_id in &pin_ids {
            separated.push_bind(*pin_id);
        }
        builder.push(") ORDER BY id");
        let attach_rows: Vec<(i64, String, Option<String>, Option<String>)> =
            builder.build_query_as().fetch_all(pool).await?;
        for (pin_id, kind, url, file_path) in attach_rows {
            let entry = attachments.entry(pin_id).or_default();
            if kind == "link" {
                if let Some(url) = url {
                    entry.0.push(url);
                }
            } else if let Some(path) = file_path {
                entry.1.push(format!("/uploads/{path}"));
            }
        }
    }
    let mut pins = Vec::new();
    for (pin_id, occurrence_id, due_date, summary) in rows {
        let Ok(day) = NaiveDate::parse_from_str(&due_date, "%Y-%m-%d") else {
            continue;
        };
        let (links, photos) = attachments.remove(&pin_id).unwrap_or_default();
        pins.push(ProfilePinJson {
            occurrence_id,
            due_date: day,
            summary,
            links,
            photos,
        });
    }
    return Ok(pins);
}
