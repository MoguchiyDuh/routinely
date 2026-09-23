use askama::Template;
use axum::Form;
use axum::Router;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use serde::Deserialize;
use serde_json::Value;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::punish;
use crate::punish::AssignTarget;

use super::WebError;
use super::WebResult;
use super::html;
use super::shell::Shell;

#[derive(Template)]
#[template(path = "punishments.html")]
pub struct PunishmentsPage {
    pub shell: Shell,
    pending: Vec<PunishVm>,
    history: Vec<PunishVm>,
    catalog: Vec<CatalogVm>,
}

pub(crate) struct PunishVm {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub source: String,
    pub status: String,
    pub cleared_at: Option<String>,
}

pub(crate) struct CatalogVm {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub active: bool,
}

#[derive(Debug, Deserialize)]
struct CatalogFormPayload {
    title: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogJsonPayload {
    title: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AssignFormPayload {
    catalog_id: Option<String>,
    custom_title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AssignJsonPayload {
    catalog_id: Option<i64>,
    custom_title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PunishListQuery {
    status: Option<String>,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/punishments", get(punishments_page))
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
) -> WebResult {
    let shell = Shell::load(&state, &auth, "penalties", &headers).await?;
    let pending = punish::list_punishments(&state.pool, auth.id, "pending").await?;
    let history = punish::list_punishments(&state.pool, auth.id, "history").await?;
    let catalog = punish::list_catalog(&state.pool).await?;
    let page = PunishmentsPage {
        shell,
        pending: pending.iter().map(punish_vm).collect(),
        history: history.iter().map(punish_vm).collect(),
        catalog: catalog.iter().map(catalog_vm).collect(),
    };
    return Ok(html(&page)?);
}

async fn clear_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(punishment_id): Path<i64>,
) -> WebResult {
    return close_and_redirect(&state, auth.id, punishment_id, false).await;
}

async fn waive_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(punishment_id): Path<i64>,
) -> WebResult {
    return close_and_redirect(&state, auth.id, punishment_id, true).await;
}

async fn close_and_redirect(
    state: &AppState,
    user_id: i64,
    punishment_id: i64,
    waive: bool,
) -> WebResult {
    return match punish::close_punishment(&state.pool, user_id, punishment_id, waive).await? {
        punish::PunishOutcome::Done => {
            let message = if waive {
                "Punishment waived"
            } else {
                "Punishment cleared"
            };
            Ok(super::shell::redirect_with_flash(
                "/punishments",
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
    let title = require_title(payload.title.as_deref())?;
    punish::add_catalog_item(
        &state.pool,
        auth.id,
        &title,
        clean_optional(payload.description.as_deref()).as_deref(),
    )
    .await?;
    return Ok(super::shell::redirect_with_flash(
        "/punishments",
        "Catalog item added",
        state.config.cookie_secure,
    ));
}

async fn deactivate_ssr(
    PageAuth(_auth): PageAuth,
    State(state): State<AppState>,
    Path(catalog_id): Path<i64>,
) -> WebResult {
    match punish::set_catalog_active(&state.pool, catalog_id, false).await? {
        false => return Err(WebError::NotFound),
        true => {
            return Ok(super::shell::redirect_with_flash(
                "/punishments",
                "Catalog item deactivated",
                state.config.cookie_secure,
            ));
        }
    }
}

async fn activate_ssr(
    PageAuth(_auth): PageAuth,
    State(state): State<AppState>,
    Path(catalog_id): Path<i64>,
) -> WebResult {
    match punish::set_catalog_active(&state.pool, catalog_id, true).await? {
        false => return Err(WebError::NotFound),
        true => {
            return Ok(super::shell::redirect_with_flash(
                "/punishments",
                "Catalog item activated",
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
    let target = target_from_form(&payload)?;
    assign_common(&state, target_user, target).await?;
    return Ok(super::shell::redirect_with_flash(
        "/punishments",
        "Punishment assigned",
        state.config.cookie_secure,
    ));
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

fn target_from_form(payload: &AssignFormPayload) -> Result<AssignTarget, WebError> {
    let catalog_id = match payload.catalog_id.as_deref() {
        None | Some("") => None,
        Some(raw) => Some(
            raw.trim()
                .parse::<i64>()
                .map_err(|_| WebError::Validation("catalog_id must be a number"))?,
        ),
    };
    let custom_title = match payload.custom_title.as_deref() {
        None | Some("") => None,
        Some(raw) => Some(raw.to_string()),
    };
    return match (catalog_id, custom_title) {
        (Some(_), Some(_)) | (None, None) => Err(WebError::Validation(
            "provide exactly one of catalog_id or custom_title",
        )),
        (Some(id), None) => Ok(AssignTarget::Catalog(id)),
        (None, Some(title)) => {
            if !punish::valid_title(&title) {
                return Err(WebError::Validation("title must be 1-200 characters"));
            }
            Ok(AssignTarget::Custom(title))
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

fn punish_vm(item: &punish::PunishmentItem) -> PunishVm {
    return PunishVm {
        id: item.id,
        title: item.title.clone().unwrap_or_else(|| {
            if item.source == "auto" && item.status == "pending" {
                "missed — no punishment available".to_string()
            } else {
                "(removed)".to_string()
            }
        }),
        description: item.description.clone(),
        source: item.source.clone(),
        status: item.status.clone(),
        cleared_at: item.cleared_at.clone(),
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
