use askama::Template;
use axum::Router;
use axum::extract::Form;
use axum::extract::Path;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::Redirect;
use axum::routing::get;
use axum::routing::post;
use chrono::NaiveDate;
use chrono::NaiveTime;
use chrono::Weekday;
use serde::Deserialize;
use serde::Serialize;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::recurrence::mask_from_weekdays;
use crate::store;

use super::WebError;
use super::WebResult;
use super::html;

#[derive(Template)]
#[template(path = "new.html")]
struct NewRoutinePage {
    error: Option<String>,
    pending_count: u64,
    nav_user_id: Option<i64>,
    nav_username: String,
}

#[derive(Template)]
#[template(path = "_routine_form.html")]
struct RoutineForm {
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RoutineFormPayload {
    title: Option<String>,
    notes: Option<String>,
    kind: Option<String>,
    monday: Option<String>,
    tuesday: Option<String>,
    wednesday: Option<String>,
    thursday: Option<String>,
    friday: Option<String>,
    saturday: Option<String>,
    sunday: Option<String>,
    per_period: Option<String>,
    period: Option<String>,
    duration: Option<String>,
    iterations: Option<String>,
    end_date: Option<String>,
    time_of_day: Option<String>,
    time_end: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RoutineJsonPayload {
    title: Option<String>,
    notes: Option<String>,
    kind: Option<String>,
    weekdays: Option<Vec<u8>>,
    per_period: Option<u32>,
    period: Option<String>,
    iterations: Option<u32>,
    end_date: Option<NaiveDate>,
    time_of_day: Option<String>,
    time_end: Option<String>,
}

#[derive(Debug, Serialize)]
struct RoutineCreated {
    id: i64,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/routines/new", get(new_routine_page))
        .route("/routines", post(create_routine_ssr))
        .route("/routines/{id}/archive", post(archive_ssr))
        .route("/api/v1/routines", post(api_create_routine))
        .route("/api/v1/routines/{id}/archive", post(api_archive));
}

async fn new_routine_page() -> WebResult {
    return Ok(html(&NewRoutinePage {
        error: None,
        pending_count: 0,
        nav_user_id: None,
        nav_username: String::new(),
    })?);
}

async fn create_routine_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Form(payload): Form<RoutineFormPayload>,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    match form_spec(&payload, today) {
        Ok(spec) => {
            store::create_routine(&state.pool, auth.id, &spec).await?;
            return Ok(Redirect::to("/day").into_response());
        }
        Err(message) => {
            return Ok(html(&RoutineForm {
                error: Some(message),
            })?);
        }
    }
}

async fn archive_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
) -> WebResult {
    let archived = store::archive_routine(&state.pool, auth.id, routine_id).await?;
    if !archived {
        return Err(WebError::NotFound);
    }
    return Ok(Redirect::to("/day").into_response());
}

async fn api_create_routine(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    axum::Json(payload): axum::Json<RoutineJsonPayload>,
) -> Result<axum::Json<RoutineCreated>, WebError> {
    let today = store::user_today(&auth.timezone);
    let raw = json_raw(&payload)?;
    let spec = build_spec(raw, today).map_err(WebError::Validation)?;
    let id = store::create_routine(&state.pool, auth.id, &spec).await?;
    return Ok(axum::Json(RoutineCreated { id }));
}

async fn api_archive(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
) -> Result<axum::response::Response, WebError> {
    let archived = store::archive_routine(&state.pool, auth.id, routine_id).await?;
    if !archived {
        return Err(WebError::NotFound);
    }
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true })),
    )
        .into_response());
}

fn form_spec(payload: &RoutineFormPayload, today: NaiveDate) -> Result<store::RoutineSpec, String> {
    let mut weekdays = Vec::new();
    if payload.monday.is_some() {
        weekdays.push(Weekday::Mon);
    }
    if payload.tuesday.is_some() {
        weekdays.push(Weekday::Tue);
    }
    if payload.wednesday.is_some() {
        weekdays.push(Weekday::Wed);
    }
    if payload.thursday.is_some() {
        weekdays.push(Weekday::Thu);
    }
    if payload.friday.is_some() {
        weekdays.push(Weekday::Fri);
    }
    if payload.saturday.is_some() {
        weekdays.push(Weekday::Sat);
    }
    if payload.sunday.is_some() {
        weekdays.push(Weekday::Sun);
    }
    let per_period = match payload.per_period.as_deref() {
        None | Some("") => None,
        Some(raw) => Some(
            raw.trim()
                .parse::<u32>()
                .map_err(|_| "per_period must be a positive number")?,
        ),
    };
    let period = match payload.period.as_deref() {
        None | Some("") => None,
        Some(raw) => Some(store::parse_period(raw.trim()).ok_or("unknown period")?),
    };
    let (iterations, end_date) = match payload.duration.as_deref() {
        Some("iterations") => {
            let raw = payload.iterations.as_deref().unwrap_or("");
            let value = raw
                .trim()
                .parse::<u32>()
                .map_err(|_| "iterations must be a positive number")?;
            (Some(value), None)
        }
        Some("end_date") => {
            let raw = payload.end_date.as_deref().unwrap_or("");
            let value = NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
                .map_err(|_| "end date must be YYYY-MM-DD")?;
            (None, Some(value))
        }
        _ => (None, None),
    };
    return build_spec(
        RawRoutine {
            title: payload.title.clone(),
            notes: payload.notes.clone(),
            kind: payload.kind.clone(),
            weekdays,
            per_period,
            period,
            iterations,
            end_date,
            time_of_day: payload.time_of_day.clone(),
            time_end: payload.time_end.clone(),
        },
        today,
    )
    .map_err(str::to_string);
}

fn json_raw(payload: &RoutineJsonPayload) -> Result<RawRoutine, WebError> {
    let mut weekdays = Vec::new();
    for bit in payload.weekdays.iter().flatten() {
        match weekday_from_index(*bit) {
            Some(day) => weekdays.push(day),
            None => return Err(WebError::Validation("weekdays must be 0-6")),
        }
    }
    let period = match payload.period.as_deref() {
        None | Some("") => None,
        Some(raw) => {
            Some(store::parse_period(raw.trim()).ok_or(WebError::Validation("unknown period"))?)
        }
    };
    return Ok(RawRoutine {
        title: payload.title.clone(),
        notes: payload.notes.clone(),
        kind: payload.kind.clone(),
        weekdays,
        per_period: payload.per_period,
        period,
        iterations: payload.iterations,
        end_date: payload.end_date,
        time_of_day: payload.time_of_day.clone(),
        time_end: payload.time_end.clone(),
    });
}

fn weekday_from_index(index: u8) -> Option<Weekday> {
    return match index {
        0 => Some(Weekday::Mon),
        1 => Some(Weekday::Tue),
        2 => Some(Weekday::Wed),
        3 => Some(Weekday::Thu),
        4 => Some(Weekday::Fri),
        5 => Some(Weekday::Sat),
        6 => Some(Weekday::Sun),
        _ => None,
    };
}

pub(crate) struct RawRoutine {
    pub title: Option<String>,
    pub notes: Option<String>,
    pub kind: Option<String>,
    pub weekdays: Vec<Weekday>,
    pub per_period: Option<u32>,
    pub period: Option<store::PeriodKind>,
    pub iterations: Option<u32>,
    pub end_date: Option<NaiveDate>,
    pub time_of_day: Option<String>,
    pub time_end: Option<String>,
}

pub(crate) fn build_spec(
    raw: RawRoutine,
    today: NaiveDate,
) -> Result<store::RoutineSpec, &'static str> {
    let Some(title) = raw.title else {
        return Err("title is required");
    };
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err("title must be 1-200 characters");
    }
    let title = title.to_string();
    let Some(kind) = raw.kind.as_deref() else {
        return Err("kind is required");
    };
    let (schedule_kind, weekdays_mask, period, per_period) = match kind {
        "weekdays" => {
            let mask = mask_from_weekdays(&raw.weekdays);
            if mask == 0 {
                return Err("select at least one weekday");
            }
            (store::ScheduleKind::Weekdays, Some(mask), None, None)
        }
        "count" => {
            let Some(per_period) = raw.per_period else {
                return Err("per_period is required");
            };
            if per_period == 0 {
                return Err("per_period must be at least 1");
            }
            let Some(period) = raw.period else {
                return Err("period is required");
            };
            (
                store::ScheduleKind::Count,
                None,
                Some(period),
                Some(per_period),
            )
        }
        _ => return Err("unknown kind"),
    };
    let (iterations, end_date) = match (raw.iterations, raw.end_date) {
        (Some(_), Some(_)) => return Err("iterations and end_date are mutually exclusive"),
        (Some(iterations), None) => {
            if iterations == 0 {
                return Err("iterations must be at least 1");
            }
            (Some(iterations), None)
        }
        (None, Some(end_date)) => {
            if end_date < today {
                return Err("end date cannot be before today");
            }
            (None, Some(end_date))
        }
        (None, None) => (None, None),
    };
    let time_of_day = validate_time(raw.time_of_day)?;
    let time_end = validate_time(raw.time_end)?;
    if time_end.is_some() && time_of_day.is_none() {
        return Err("time end requires time of day");
    }
    return Ok(store::RoutineSpec {
        title,
        notes: raw.notes,
        kind: schedule_kind,
        weekdays_mask,
        period,
        per_period,
        iterations,
        end_date,
        time_of_day,
        time_end,
        start_date: today,
    });
}

fn validate_time(raw: Option<String>) -> Result<Option<String>, &'static str> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    NaiveTime::parse_from_str(trimmed, "%H:%M").map_err(|_| "invalid time")?;
    return Ok(Some(trimmed.to_string()));
}
