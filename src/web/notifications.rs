use askama::Template;
use axum::Router;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::response::IntoResponse;
use axum::response::Redirect;
use axum::routing::delete;
use axum::routing::get;
use axum::routing::post;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::notify;
use crate::store;

use super::WebError;
use super::WebResult;
use super::html;
use super::is_htmx;
use super::shell::Shell;

const PAGE_TRIGGER: &str = "notifications-changed";

#[derive(Debug, Deserialize)]
struct ListQuery {
    limit: Option<i64>,
    before_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ReminderQuery {
    target_type: Option<String>,
    target_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ReminderPayload {
    target_type: Option<String>,
    target_id: Option<i64>,
    offset_value: Option<i64>,
    offset_unit: Option<String>,
    at_time: Option<String>,
}

impl From<store::ReminderError> for WebError {
    fn from(value: store::ReminderError) -> Self {
        return match value {
            store::ReminderError::Invalid(message) => WebError::Validation(message),
            store::ReminderError::NotFound => WebError::NotFound,
            store::ReminderError::Duplicate => WebError::Conflict,
            store::ReminderError::Db(error) => WebError::Db(error),
        };
    }
}

#[derive(Debug, Deserialize)]
struct PageQuery {
    before_id: Option<i64>,
}

#[derive(Template)]
#[template(path = "notifications.html")]
pub struct NotificationsPage {
    pub shell: Shell,
    items: Vec<NotificationVm>,
    next_before_id: Option<i64>,
}

#[derive(Template)]
#[template(path = "_notification_items.html")]
struct NotificationItems {
    items: Vec<NotificationVm>,
    next_before_id: Option<i64>,
}

#[derive(Template)]
#[template(path = "_notification.html")]
struct NotificationItem {
    n: NotificationVm,
}

#[derive(Template)]
#[template(path = "_bell_badge.html")]
struct BellBadge {
    count: i64,
}

struct NotificationVm {
    id: i64,
    icon: &'static str,
    title: String,
    body: Option<String>,
    when_label: String,
    when_full: String,
    unread: bool,
    has_url: bool,
}

pub struct ReminderRowVm {
    pub value: String,
    pub unit: &'static str,
    pub at_time: String,
    pub show_time: bool,
}

pub struct RemindersVm {
    pub rows: Vec<ReminderRowVm>,
    pub blank: ReminderRowVm,
    pub json: String,
    pub defaults_timed: Option<String>,
    pub defaults_untimed: Option<String>,
    pub timed: bool,
    pub time_source: &'static str,
    pub error: Option<&'static str>,
    pub max: usize,
}

const DEFAULT_AT_TIME: &str = "09:00";

fn static_unit(unit: &str) -> &'static str {
    return store::REMINDER_UNITS
        .iter()
        .copied()
        .find(|candidate| *candidate == unit)
        .unwrap_or("minutes");
}

fn row_vm(spec: &store::ReminderSpec, timed: bool) -> ReminderRowVm {
    let unit = static_unit(&spec.offset_unit);
    return ReminderRowVm {
        value: spec.offset_value.to_string(),
        unit,
        at_time: spec
            .at_time
            .clone()
            .unwrap_or_else(|| DEFAULT_AT_TIME.to_string()),
        show_time: !(timed && matches!(unit, "minutes" | "hours")),
    };
}

impl RemindersVm {
    pub fn new(
        specs: &[store::ReminderSpec],
        timed: bool,
        time_source: &'static str,
    ) -> RemindersVm {
        let blank = store::ReminderSpec {
            offset_value: 10,
            offset_unit: "minutes".to_string(),
            at_time: None,
        };
        return RemindersVm {
            rows: specs.iter().map(|spec| row_vm(spec, timed)).collect(),
            blank: row_vm(&blank, timed),
            json: serde_json::to_string(specs).unwrap_or_else(|_| "[]".to_string()),
            defaults_timed: None,
            defaults_untimed: None,
            timed,
            time_source,
            error: None,
            max: store::MAX_REMINDERS_PER_TARGET,
        };
    }

    pub fn from_rows(
        rows: &[store::ReminderRow],
        timed: bool,
        time_source: &'static str,
    ) -> RemindersVm {
        let specs: Vec<store::ReminderSpec> = rows
            .iter()
            .map(|row| store::ReminderSpec {
                offset_value: row.offset_value,
                offset_unit: row.offset_unit.clone(),
                at_time: row.at_time.clone(),
            })
            .collect();
        return RemindersVm::new(&specs, timed, time_source);
    }

    pub fn from_json(raw: Option<&str>, timed: bool, time_source: &'static str) -> RemindersVm {
        let trimmed = raw.map(str::trim).filter(|value| !value.is_empty());
        let Some(trimmed) = trimmed else {
            return RemindersVm::new(&[], timed, time_source);
        };
        return match serde_json::from_str::<Vec<store::ReminderSpec>>(trimmed) {
            Ok(specs) => RemindersVm::new(&specs, timed, time_source),
            Err(_) => RemindersVm::new(&[], timed, time_source).with_error("invalid reminders"),
        };
    }

    pub fn with_defaults(
        mut self,
        prefs: &notify::NotificationPrefs,
        target_type: &str,
    ) -> RemindersVm {
        let encode = |timed: bool| {
            serde_json::to_string(&store::default_reminder_specs(prefs, target_type, timed))
                .unwrap_or_else(|_| "[]".to_string())
        };
        self.defaults_timed = Some(encode(true));
        self.defaults_untimed = Some(encode(false));
        return self;
    }

    pub fn with_error(mut self, message: &'static str) -> RemindersVm {
        self.error = Some(message);
        return self;
    }
}

pub fn reminder_error_message(error: &store::ReminderError) -> &'static str {
    return match error {
        store::ReminderError::Invalid(message) => message,
        store::ReminderError::NotFound => "reminder target not found",
        store::ReminderError::Duplicate => "duplicate reminder",
        store::ReminderError::Db(_) => "could not save reminders",
    };
}

pub fn reminder_label(row: &store::ReminderRow) -> String {
    let unit = if row.offset_value == 1 {
        row.offset_unit.trim_end_matches('s')
    } else {
        row.offset_unit.as_str()
    };
    return match (row.at_time.as_deref(), row.offset_value) {
        (None, 0) => "at deadline".to_string(),
        (None, value) => format!("{value} {unit} before"),
        (Some(time), 0) => format!("same day at {time}"),
        (Some(time), value) => format!("{value} {unit} before at {time}"),
    };
}

fn kind_icon(kind: &str) -> &'static str {
    return match kind {
        "routine_reminder" => "repeat",
        "todo_reminder" => "flag",
        "day_end" => "moon",
        "penalty_issued" => "gavel",
        "penalty_resolved" => "circle-check",
        _ => "bell",
    };
}

fn relative_label(created: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - created).num_seconds();
    return match seconds {
        ..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86400 => format!("{} h ago", seconds / 3600),
        _ => format!("{} d ago", seconds / 86400),
    };
}

fn notification_vm(
    row: notify::NotificationRow,
    timezone: &str,
    now: DateTime<Utc>,
) -> NotificationVm {
    let tz = store::user_timezone(timezone);
    let (when_label, when_full) = match DateTime::parse_from_rfc3339(&row.created_at) {
        Ok(parsed) => {
            let created = parsed.with_timezone(&Utc);
            (
                relative_label(created, now),
                created
                    .with_timezone(&tz)
                    .format("%Y-%m-%d %H:%M")
                    .to_string(),
            )
        }
        Err(_) => (String::new(), String::new()),
    };
    return NotificationVm {
        id: row.id,
        icon: kind_icon(&row.kind),
        title: row.title,
        body: row.body,
        when_label,
        when_full,
        unread: row.unread,
        has_url: row.url.as_deref().is_some_and(is_internal_path),
    };
}

fn is_internal_path(url: &str) -> bool {
    return url.starts_with('/') && !url.starts_with("//") && !url.contains('\\');
}

async fn load_page(
    state: &AppState,
    user_id: i64,
    timezone: &str,
    before_id: Option<i64>,
) -> Result<(Vec<NotificationVm>, Option<i64>), sqlx::Error> {
    let size = notify::DEFAULT_PAGE_SIZE;
    let mut rows = notify::list(&state.pool, user_id, size + 1, before_id).await?;
    let has_more = i64::try_from(rows.len()).is_ok_and(|len| len > size);
    rows.truncate(usize::try_from(size).unwrap_or(0));
    let next_before_id = if has_more {
        rows.last().map(|row| row.id)
    } else {
        None
    };
    let now = Utc::now();
    let items = rows
        .into_iter()
        .map(|row| notification_vm(row, timezone, now))
        .collect();
    return Ok((items, next_before_id));
}

fn with_trigger(mut response: axum::response::Response) -> axum::response::Response {
    response
        .headers_mut()
        .insert("HX-Trigger", HeaderValue::from_static(PAGE_TRIGGER));
    return response;
}

async fn page_view(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> WebResult {
    let (items, next_before_id) =
        load_page(&state, auth.id, &auth.timezone, query.before_id).await?;
    if is_htmx(&headers) && query.before_id.is_some() {
        return Ok(html(&NotificationItems {
            items,
            next_before_id,
        })?);
    }
    let shell = Shell::load(&state, &auth, "notifications", &headers).await?;
    return Ok(html(&NotificationsPage {
        shell,
        items,
        next_before_id,
    })?);
}

async fn badge_view(PageAuth(auth): PageAuth, State(state): State<AppState>) -> WebResult {
    let count = notify::unread_count(&state.pool, auth.id).await?;
    return Ok(html(&BellBadge { count })?);
}

async fn read_all_ssr(PageAuth(auth): PageAuth, State(state): State<AppState>) -> WebResult {
    notify::mark_all_read(&state.pool, auth.id).await?;
    return Ok(Redirect::to("/notifications").into_response());
}

async fn read_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(notification_id): Path<i64>,
    headers: HeaderMap,
) -> WebResult {
    let found = notify::mark_read(&state.pool, auth.id, notification_id).await?;
    if !found {
        return Err(WebError::NotFound);
    }
    if !is_htmx(&headers) {
        return Ok(Redirect::to("/notifications").into_response());
    }
    let Some(row) = notify::find(&state.pool, auth.id, notification_id).await? else {
        return Err(WebError::NotFound);
    };
    let n = notification_vm(row, &auth.timezone, Utc::now());
    return Ok(with_trigger(html(&NotificationItem { n })?));
}

async fn delete_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(notification_id): Path<i64>,
    headers: HeaderMap,
) -> WebResult {
    let found = notify::dismiss(&state.pool, auth.id, notification_id).await?;
    if !found {
        return Err(WebError::NotFound);
    }
    if !is_htmx(&headers) {
        return Ok(Redirect::to("/notifications").into_response());
    }
    return Ok(with_trigger(axum::response::Html("").into_response()));
}

async fn open_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(notification_id): Path<i64>,
) -> WebResult {
    let Some(row) = notify::find(&state.pool, auth.id, notification_id).await? else {
        return Err(WebError::NotFound);
    };
    notify::mark_read(&state.pool, auth.id, notification_id).await?;
    let target = row
        .url
        .as_deref()
        .filter(|url| is_internal_path(url))
        .unwrap_or("/notifications");
    return Ok(Redirect::to(target).into_response());
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/notifications", get(page_view))
        .route("/notifications/badge", get(badge_view))
        .route("/notifications/read", post(read_all_ssr))
        .route("/notifications/{id}/read", post(read_ssr))
        .route("/notifications/{id}/open", get(open_ssr))
        .route("/notifications/{id}/delete", post(delete_ssr))
        .route("/api/v1/notifications", get(api_list))
        .route("/api/v1/notifications/unread_count", get(api_unread_count))
        .route("/api/v1/notifications/read", post(api_read_all))
        .route("/api/v1/notifications/{id}/read", post(api_read))
        .route("/api/v1/notifications/{id}", delete(api_delete))
        .route(
            "/api/v1/notifications/prefs",
            get(api_get_prefs).put(api_put_prefs),
        )
        .route(
            "/api/v1/reminders",
            get(api_list_reminders).post(api_create_reminder),
        )
        .route("/api/v1/reminders/{id}", delete(api_delete_reminder));
}

async fn api_list(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<axum::Json<serde_json::Value>, WebError> {
    let limit = query
        .limit
        .unwrap_or(notify::DEFAULT_PAGE_SIZE)
        .clamp(1, notify::MAX_PAGE_SIZE);
    let items = notify::list(&state.pool, auth.id, limit, query.before_id).await?;
    let next_before_id = if i64::try_from(items.len()).is_ok_and(|len| len == limit) {
        items.last().map(|item| item.id)
    } else {
        None
    };
    return Ok(axum::Json(
        serde_json::json!({ "items": items, "next_before_id": next_before_id }),
    ));
}

async fn api_unread_count(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
) -> Result<axum::Json<serde_json::Value>, WebError> {
    let count = notify::unread_count(&state.pool, auth.id).await?;
    return Ok(axum::Json(serde_json::json!({ "count": count })));
}

async fn api_read_all(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
) -> Result<axum::Json<serde_json::Value>, WebError> {
    let updated = notify::mark_all_read(&state.pool, auth.id).await?;
    return Ok(axum::Json(
        serde_json::json!({ "ok": true, "updated": updated }),
    ));
}

async fn api_read(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(notification_id): Path<i64>,
) -> Result<axum::Json<serde_json::Value>, WebError> {
    let found = notify::mark_read(&state.pool, auth.id, notification_id).await?;
    if !found {
        return Err(WebError::NotFound);
    }
    return Ok(axum::Json(serde_json::json!({ "ok": true })));
}

async fn api_delete(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(notification_id): Path<i64>,
) -> Result<axum::Json<serde_json::Value>, WebError> {
    let found = notify::dismiss(&state.pool, auth.id, notification_id).await?;
    if !found {
        return Err(WebError::NotFound);
    }
    return Ok(axum::Json(serde_json::json!({ "ok": true })));
}

async fn api_get_prefs(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
) -> Result<axum::Json<notify::NotificationPrefs>, WebError> {
    return Ok(axum::Json(notify::get_prefs(&state.pool, auth.id).await?));
}

async fn api_put_prefs(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    axum::Json(mut payload): axum::Json<notify::NotificationPrefs>,
) -> Result<axum::Json<notify::NotificationPrefs>, WebError> {
    let stored = notify::get_prefs(&state.pool, auth.id).await?;
    notify::apply_cleared_times(&mut payload, &stored);
    let prefs = notify::validate_prefs(&payload).map_err(WebError::Validation)?;
    notify::save_prefs(&state.pool, auth.id, &prefs).await?;
    return Ok(axum::Json(prefs));
}

async fn api_list_reminders(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Query(query): Query<ReminderQuery>,
) -> Result<axum::Json<serde_json::Value>, WebError> {
    if let Some(target_type) = query.target_type.as_deref()
        && !store::REMINDER_TARGETS.contains(&target_type)
    {
        return Err(WebError::Validation("unknown reminder target"));
    }
    let items = store::list_user_reminders(
        &state.pool,
        auth.id,
        query.target_type.as_deref(),
        query.target_id,
    )
    .await?;
    return Ok(axum::Json(serde_json::json!({ "items": items })));
}

async fn api_create_reminder(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    axum::Json(payload): axum::Json<ReminderPayload>,
) -> Result<axum::Json<serde_json::Value>, WebError> {
    let Some(target_type) = payload.target_type.as_deref() else {
        return Err(WebError::Validation("target_type is required"));
    };
    let Some(target_id) = payload.target_id else {
        return Err(WebError::Validation("target_id is required"));
    };
    let Some(offset_value) = payload.offset_value else {
        return Err(WebError::Validation("offset_value is required"));
    };
    let Some(offset_unit) = payload.offset_unit else {
        return Err(WebError::Validation("offset_unit is required"));
    };
    let spec = store::ReminderSpec {
        offset_value,
        offset_unit,
        at_time: payload.at_time,
    };
    let id = store::add_reminder(&state.pool, auth.id, target_type, target_id, &spec).await?;
    return Ok(axum::Json(serde_json::json!({ "id": id })));
}

async fn api_delete_reminder(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(reminder_id): Path<i64>,
) -> Result<axum::Json<serde_json::Value>, WebError> {
    let deleted = store::delete_reminder(&state.pool, auth.id, reminder_id).await?;
    if !deleted {
        return Err(WebError::NotFound);
    }
    return Ok(axum::Json(serde_json::json!({ "ok": true })));
}
