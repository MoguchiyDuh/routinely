use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::Request;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::HeaderValue;
use axum::http::header::CACHE_CONTROL;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use sqlx::SqlitePool;
use tower_http::services::ServeDir;

use crate::auth::routes;
use crate::config::Config;
use crate::store::UPLOADS_DIR;
use crate::web;

const MAX_BODY_BYTES: usize = 12 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Config,
}

pub async fn healthz(State(state): State<AppState>) -> (StatusCode, &'static str) {
    let db = sqlx::query("SELECT 1").execute(&state.pool).await;
    if db.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "database unavailable");
    }
    return (StatusCode::OK, "ok");
}

async fn no_cache(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    return response;
}

pub fn build_router(state: AppState) -> Router {
    let static_files = Router::new()
        .fallback_service(ServeDir::new("static"))
        .layer(middleware::from_fn(no_cache));
    return Router::new()
        .route("/healthz", get(healthz))
        .merge(routes::router())
        .merge(web::router())
        .nest_service("/static", static_files)
        .nest_service("/uploads", ServeDir::new(UPLOADS_DIR))
        .layer(middleware::from_fn(web::html_errors))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state);
}
