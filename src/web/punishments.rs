use askama::Template;
use axum::Form;
use axum::Router;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use serde::Deserialize;
use serde_json::Value;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::auth::session::SessionUser;
use crate::punish;
use crate::punish::AssignTarget;
use crate::store;

use super::AvatarVm;
use super::WebError;
use super::WebResult;
use super::html;
use super::shell::Shell;

#[derive(Template)]
#[template(path = "punishments.html")]
pub struct PunishmentsPage {
    pub shell: Shell,
    tab: &'static str,
    pending: Vec<PenaltyVm>,
    history: Vec<HistoryVm>,
    everyone: Vec<GroupVm>,
    members: Vec<MemberVm>,
    catalog: Vec<CatalogVm>,
    assign_error: Option<&'static str>,
}

pub(crate) struct PenaltyVm {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub source_label: String,
    pub assigned_date: String,
}

pub(crate) struct HistoryVm {
    pub title: String,
    pub source_label: String,
    pub pill_class: &'static str,
    pub pill_word: &'static str,
    pub pill_icon: &'static str,
    pub date: String,
}

pub(crate) struct GroupVm {
    pub user_id: i64,
    pub username: String,
    pub avatar: AvatarVm,
    pub count: u64,
    pub is_self: bool,
    pub items: Vec<PenaltyVm>,
}

pub(crate) struct MemberVm {
    pub id: i64,
    pub username: String,
}

pub(crate) struct CatalogVm {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub active: bool,
}

#[derive(Debug, Deserialize)]
struct PageQuery {
    tab: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogFormPayload {
    title: Option<String>,
    description: Option<String>,
    tab: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogJsonPayload {
    title: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TabForm {
    tab: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AssignFormPayload {
    tab: Option<String>,
    catalog_id: Option<String>,
    custom_title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AssignJsonPayload {
    catalog_id: Option<i64>,
    custom_title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AssignPickForm {
    user_id: Option<String>,
    catalog_id: Option<String>,
    custom_title: Option<String>,
    tab: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PunishListQuery {
    status: Option<String>,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/punishments", get(punishments_page))
        .route("/punishments/assign", post(assign_pick_ssr))
        .route("/punishments/{id}/clear", post(clear_ssr))
        .route("/punishments/{id}/waive", post(waive_ssr))
        .route("/catalog", post(add_catalog_ssr))
        .route("/catalog/{id}/deactivate", post(deactivate_ssr))
        .route("/catalog/{id}/activate", post(activate_ssr))
        .route("/users/{id}/punishments", post(assign_ssr))
        .route("/api/v1/punishments", get(api_list))
        .route("/api/v1/punishments/{id}/clear", post(api_clear))
        .route("/api/v1/punishments/{id}/waive", post(api_waive))
        .route("/api/v1/catalog", get(api_catalog).post(api_add_catalog))
        .route("/api/v1/catalog/{id}/deactivate", post(api_deactivate))
        .route("/api/v1/catalog/{id}/activate", post(api_activate))
        .route("/api/v1/users/{id}/punishments", post(api_assign));
}

async fn punishments_page(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<PageQuery>,
) -> WebResult {
    let tab = tab_from_raw(query.tab.as_deref());
    let page = build_page(&state, &auth, &headers, tab, None).await?;
    return Ok(html(&page)?);
}

async fn build_page(
    state: &AppState,
    auth: &SessionUser,
    headers: &axum::http::HeaderMap,
    tab: &'static str,
    assign_error: Option<&'static str>,
) -> Result<PunishmentsPage, WebError> {
    let shell = Shell::load(state, auth, "penalties", headers).await?;
    let mut pending: Vec<PenaltyVm> = Vec::new();
    let mut history: Vec<HistoryVm> = Vec::new();
    let mut everyone: Vec<GroupVm> = Vec::new();
    let mut members: Vec<MemberVm> = Vec::new();
    let mut catalog: Vec<CatalogVm> = Vec::new();
    if tab == "mine" {
        let details = punish::list_user_details(&state.pool, auth.id).await?;
        for detail in &details {
            if detail.status == "pending" {
                pending.push(penalty_vm(detail, &auth.timezone));
            } else {
                history.push(history_vm(detail, &auth.timezone));
            }
        }
        history.sort_by(|a, b| b.date.cmp(&a.date));
    } else if tab == "everyone" {
        let details = punish::list_pending_details(&state.pool).await?;
        for detail in &details {
            let same_group =
                matches!(everyone.last(), Some(group) if group.user_id == detail.user_id);
            if !same_group {
                let avatar = AvatarVm::load(&state.pool, detail.user_id).await?;
                everyone.push(GroupVm {
                    user_id: detail.user_id,
                    username: detail.username.clone(),
                    avatar,
                    count: 0,
                    is_self: detail.user_id == auth.id,
                    items: Vec::new(),
                });
            }
            if let Some(group) = everyone.last_mut() {
                group.items.push(penalty_vm(detail, &auth.timezone));
            }
        }
        for group in &mut everyone {
            group.count = u64::try_from(group.items.len()).unwrap_or(0);
        }
        let member_rows = punish::list_members(&state.pool).await?;
        members = member_rows
            .into_iter()
            .map(|(id, username)| MemberVm { id, username })
            .collect();
        catalog = load_catalog(state).await?;
    } else {
        catalog = load_catalog(state).await?;
    }
    return Ok(PunishmentsPage {
        shell,
        tab,
        pending,
        history,
        everyone,
        members,
        catalog,
        assign_error,
    });
}

async fn load_catalog(state: &AppState) -> Result<Vec<CatalogVm>, WebError> {
    let items = punish::list_catalog(&state.pool).await?;
    return Ok(items.iter().map(catalog_vm).collect());
}

async fn clear_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(punishment_id): Path<i64>,
    Form(form): Form<TabForm>,
) -> WebResult {
    return close_and_redirect(&state, auth.id, punishment_id, false, form.tab.as_deref()).await;
}

async fn waive_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(punishment_id): Path<i64>,
    Form(form): Form<TabForm>,
) -> WebResult {
    return close_and_redirect(&state, auth.id, punishment_id, true, form.tab.as_deref()).await;
}

async fn close_and_redirect(
    state: &AppState,
    user_id: i64,
    punishment_id: i64,
    waive: bool,
    tab_raw: Option<&str>,
) -> WebResult {
    let tab = tab_from_raw(tab_raw);
    return match punish::close_punishment(&state.pool, user_id, punishment_id, waive).await? {
        punish::PunishOutcome::Done => {
            let message = if waive {
                "Penalty waived"
            } else {
                "Penalty cleared"
            };
            Ok(super::shell::redirect_with_flash(
                &tab_target(tab),
                message,
                state.config.cookie_secure,
            ))
        }
        punish::PunishOutcome::NotPending => Err(WebError::Conflict),
        punish::PunishOutcome::Missing => Err(WebError::NotFound),
    };
}

async fn add_catalog_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Form(payload): Form<CatalogFormPayload>,
) -> WebResult {
    let tab = tab_from_raw(payload.tab.as_deref());
    let title = require_title(payload.title.as_deref())?;
    punish::add_catalog_item(
        &state.pool,
        auth.id,
        &title,
        clean_optional(payload.description.as_deref()).as_deref(),
    )
    .await?;
    return Ok(super::shell::redirect_with_flash(
        &tab_target(tab),
        "Catalog item added",
        state.config.cookie_secure,
    ));
}

async fn deactivate_ssr(
    PageAuth(_auth): PageAuth,
    State(state): State<AppState>,
    Path(catalog_id): Path<i64>,
    Form(form): Form<TabForm>,
) -> WebResult {
    return set_active_and_redirect(&state, catalog_id, false, form.tab.as_deref()).await;
}

async fn activate_ssr(
    PageAuth(_auth): PageAuth,
    State(state): State<AppState>,
    Path(catalog_id): Path<i64>,
    Form(form): Form<TabForm>,
) -> WebResult {
    return set_active_and_redirect(&state, catalog_id, true, form.tab.as_deref()).await;
}

async fn set_active_and_redirect(
    state: &AppState,
    catalog_id: i64,
    active: bool,
    tab_raw: Option<&str>,
) -> WebResult {
    let tab = tab_from_raw(tab_raw);
    match punish::set_catalog_active(&state.pool, catalog_id, active).await? {
        false => return Err(WebError::NotFound),
        true => {
            let message = if active {
                "Catalog item activated"
            } else {
                "Catalog item deactivated"
            };
            return Ok(super::shell::redirect_with_flash(
                &tab_target(tab),
                message,
                state.config.cookie_secure,
            ));
        }
    }
}

async fn assign_ssr(
    PageAuth(_auth): PageAuth,
    State(state): State<AppState>,
    Path(target_user): Path<i64>,
    Form(payload): Form<AssignFormPayload>,
) -> WebResult {
    let tab = tab_from_raw(payload.tab.as_deref());
    let target = target_from_form(
        payload.catalog_id.as_deref(),
        payload.custom_title.as_deref(),
    )
    .map_err(WebError::Validation)?;
    assign_common(&state, target_user, target).await?;
    return Ok(super::shell::redirect_with_flash(
        &tab_target(tab),
        "Penalty assigned",
        state.config.cookie_secure,
    ));
}

async fn assign_pick_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Form(form): Form<AssignPickForm>,
) -> WebResult {
    let tab = tab_from_raw(form.tab.as_deref());
    let target_user = form
        .user_id
        .as_deref()
        .and_then(|raw| raw.trim().parse::<i64>().ok());
    let Some(target_user) = target_user else {
        return render_error_page(&state, &auth, &headers, tab, "Choose a member").await;
    };
    let target = match target_from_form(form.catalog_id.as_deref(), form.custom_title.as_deref()) {
        Ok(target) => target,
        Err(message) => {
            let ui_message = match message {
                "title must be 1-200 characters" => "Use 1 to 200 characters for the title",
                _ => "Pick a catalog item or enter a custom title",
            };
            return render_error_page(&state, &auth, &headers, tab, ui_message).await;
        }
    };
    return match assign_common(&state, target_user, target).await {
        Ok(_) => Ok(super::shell::redirect_with_flash(
            &tab_target(tab),
            "Penalty assigned",
            state.config.cookie_secure,
        )),
        Err(WebError::NotFound) => {
            render_error_page(&state, &auth, &headers, tab, "Choose a member").await
        }
        Err(WebError::Validation(_)) => {
            render_error_page(
                &state,
                &auth,
                &headers,
                tab,
                "That catalog item is not available",
            )
            .await
        }
        Err(err) => Err(err),
    };
}

async fn render_error_page(
    state: &AppState,
    auth: &SessionUser,
    headers: &axum::http::HeaderMap,
    tab: &'static str,
    message: &'static str,
) -> WebResult {
    let page = build_page(state, auth, headers, tab, Some(message)).await?;
    let body = page.render()?;
    return Ok((StatusCode::UNPROCESSABLE_ENTITY, axum::response::Html(body)).into_response());
}

async fn api_list(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Query(query): Query<PunishListQuery>,
) -> Result<axum::Json<Value>, WebError> {
    let status = query.status.as_deref().unwrap_or("pending");
    let valid = matches!(status, "pending" | "cleared" | "waived" | "all");
    if !valid {
        return Err(WebError::Validation("unknown status"));
    }
    let items = punish::list_punishments(&state.pool, auth.id, status).await?;
    return Ok(axum::Json(Value::Array(
        items.iter().map(punish_json).collect(),
    )));
}

async fn api_clear(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(punishment_id): Path<i64>,
) -> Result<Response, WebError> {
    return api_close(&state, auth.id, punishment_id, false).await;
}

async fn api_waive(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(punishment_id): Path<i64>,
) -> Result<Response, WebError> {
    return api_close(&state, auth.id, punishment_id, true).await;
}

async fn api_close(
    state: &AppState,
    user_id: i64,
    punishment_id: i64,
    waive: bool,
) -> Result<Response, WebError> {
    match punish::close_punishment(&state.pool, user_id, punishment_id, waive).await? {
        punish::PunishOutcome::Done => {
            return Ok((
                axum::http::StatusCode::OK,
                axum::Json(serde_json::json!({ "ok": true })),
            )
                .into_response());
        }
        punish::PunishOutcome::NotPending => return Err(WebError::Conflict),
        punish::PunishOutcome::Missing => return Err(WebError::NotFound),
    }
}

async fn api_catalog(
    ApiAuth(_auth): ApiAuth,
    State(state): State<AppState>,
) -> Result<axum::Json<Value>, WebError> {
    let items = punish::list_catalog(&state.pool).await?;
    return Ok(axum::Json(Value::Array(
        items.iter().map(catalog_json).collect(),
    )));
}

async fn api_add_catalog(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    axum::Json(payload): axum::Json<CatalogJsonPayload>,
) -> Result<axum::Json<Value>, WebError> {
    let title = require_title(payload.title.as_deref())?;
    let id = punish::add_catalog_item(
        &state.pool,
        auth.id,
        &title,
        clean_optional(payload.description.as_deref()).as_deref(),
    )
    .await?;
    return Ok(axum::Json(serde_json::json!({ "id": id })));
}

async fn api_set_active(
    state: &AppState,
    catalog_id: i64,
    active: bool,
) -> Result<Response, WebError> {
    let updated = punish::set_catalog_active(&state.pool, catalog_id, active).await?;
    if !updated {
        return Err(WebError::NotFound);
    }
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true })),
    )
        .into_response());
}

async fn api_deactivate(
    ApiAuth(_auth): ApiAuth,
    State(state): State<AppState>,
    Path(catalog_id): Path<i64>,
) -> Result<Response, WebError> {
    return api_set_active(&state, catalog_id, false).await;
}

async fn api_activate(
    ApiAuth(_auth): ApiAuth,
    State(state): State<AppState>,
    Path(catalog_id): Path<i64>,
) -> Result<Response, WebError> {
    return api_set_active(&state, catalog_id, true).await;
}

async fn api_assign(
    ApiAuth(_auth): ApiAuth,
    State(state): State<AppState>,
    Path(target_user): Path<i64>,
    axum::Json(payload): axum::Json<AssignJsonPayload>,
) -> Result<axum::Json<Value>, WebError> {
    let target = match (payload.catalog_id, payload.custom_title) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(WebError::Validation(
                "provide exactly one of catalog_id or custom_title",
            ));
        }
        (Some(catalog_id), None) => AssignTarget::Catalog(catalog_id),
        (None, Some(custom_title)) => {
            if !punish::valid_title(&custom_title) {
                return Err(WebError::Validation("title must be 1-200 characters"));
            }
            AssignTarget::Custom(custom_title)
        }
    };
    let id = assign_common(&state, target_user, target).await?;
    return Ok(axum::Json(serde_json::json!({ "id": id })));
}

async fn assign_common(
    state: &AppState,
    target_user: i64,
    target: AssignTarget,
) -> Result<i64, WebError> {
    if !punish::user_exists(&state.pool, target_user).await? {
        return Err(WebError::NotFound);
    }
    if let AssignTarget::Catalog(catalog_id) = &target {
        match punish::catalog_active(&state.pool, *catalog_id).await? {
            None => return Err(WebError::NotFound),
            Some(false) => return Err(WebError::Validation("catalog item is not active")),
            Some(true) => {}
        }
    }
    let id = punish::manual_assign(&state.pool, target_user, target).await?;
    return Ok(id);
}

fn tab_from_raw(raw: Option<&str>) -> &'static str {
    return match raw {
        Some("everyone") => "everyone",
        Some("catalog") => "catalog",
        _ => "mine",
    };
}

fn tab_target(tab: &str) -> String {
    return format!("/punishments?tab={tab}");
}

fn target_from_form(
    catalog_raw: Option<&str>,
    custom_raw: Option<&str>,
) -> Result<AssignTarget, &'static str> {
    let catalog_id = match catalog_raw {
        None | Some("") => None,
        Some(raw) => Some(
            raw.trim()
                .parse::<i64>()
                .map_err(|_| "catalog_id must be a number")?,
        ),
    };
    let custom_title = match custom_raw {
        None | Some("") => None,
        Some(raw) => Some(raw.to_string()),
    };
    return match (catalog_id, custom_title) {
        (Some(_), Some(_)) | (None, None) => {
            Err("provide exactly one of catalog_id or custom_title")
        }
        (Some(id), None) => Ok(AssignTarget::Catalog(id)),
        (None, Some(title)) => {
            if !punish::valid_title(&title) {
                return Err("title must be 1-200 characters");
            }
            return Ok(AssignTarget::Custom(title));
        }
    };
}

fn require_title(raw: Option<&str>) -> Result<String, WebError> {
    let Some(raw) = raw else {
        return Err(WebError::Validation("title is required"));
    };
    if !punish::valid_title(raw) {
        return Err(WebError::Validation("title must be 1-200 characters"));
    }
    return Ok(raw.trim().to_string());
}

fn clean_optional(raw: Option<&str>) -> Option<String> {
    return raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
}

fn penalty_vm(item: &punish::PunishmentDetail, timezone: &str) -> PenaltyVm {
    return PenaltyVm {
        id: item.id,
        title: fallback_title(item),
        description: item.description.clone(),
        source_label: source_label(item),
        assigned_date: day_of(&item.assigned_at, timezone),
    };
}

fn source_label(item: &punish::PunishmentDetail) -> String {
    return if item.source == "auto" {
        match (&item.missed_routine, &item.missed_date, &item.period_start) {
            (Some(routine), Some(date), _) => format!("Auto · missed {routine} on {date}"),
            (Some(routine), None, Some(start)) => {
                let span = match item.missed_period.as_deref() {
                    Some("day") => format!("on {start}"),
                    Some("month") => format!("in the month of {start}"),
                    Some("year") => format!("in the year from {start}"),
                    _ => format!("in the week of {start}"),
                };
                format!("Auto · missed {routine} goal {span}")
            }
            _ => "Auto".to_string(),
        }
    } else {
        "Assigned".to_string()
    };
}

fn history_vm(item: &punish::PunishmentDetail, timezone: &str) -> HistoryVm {
    let (pill_class, pill_word, pill_icon) = if item.status == "cleared" {
        ("pill-done", "Cleared", "circle-check")
    } else {
        ("pill-planned", "Waived", "x")
    };
    return HistoryVm {
        title: fallback_title(item),
        source_label: source_label(item),
        pill_class,
        pill_word,
        pill_icon,
        date: match &item.cleared_at {
            Some(ts) => day_of(ts, timezone),
            None => day_of(&item.assigned_at, timezone),
        },
    };
}

fn fallback_title(item: &punish::PunishmentDetail) -> String {
    return item.title.clone().unwrap_or_else(|| {
        if item.source == "auto" && item.status == "pending" {
            "Missed, no penalty drawn".to_string()
        } else {
            "(removed)".to_string()
        }
    });
}

fn day_of(ts: &str, timezone: &str) -> String {
    return match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(at) => at
            .with_timezone(&store::user_timezone(timezone))
            .date_naive()
            .to_string(),
        Err(_) => ts.get(..10).unwrap_or(ts).to_string(),
    };
}

fn catalog_vm(item: &punish::CatalogItem) -> CatalogVm {
    return CatalogVm {
        id: item.id,
        title: item.title.clone(),
        description: item.description.clone(),
        active: item.active,
    };
}

fn punish_json(item: &punish::PunishmentItem) -> Value {
    return serde_json::json!({
        "id": item.id,
        "user_id": item.user_id,
        "title": item.title,
        "description": item.description,
        "source": item.source,
        "status": item.status,
        "assigned_at": item.assigned_at,
        "cleared_at": item.cleared_at,
    });
}

fn catalog_json(item: &punish::CatalogItem) -> Value {
    return serde_json::json!({
        "id": item.id,
        "title": item.title,
        "description": item.description,
        "active": item.active,
        "created_by": item.created_by,
        "created_at": item.created_at,
    });
}
