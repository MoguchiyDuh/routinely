use axum::Router;
use axum::extract::DefaultBodyLimit;
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

pub async fn healthz() -> &'static str {
    return "ok";
}

pub fn build_router(state: AppState) -> Router {
    return Router::new()
        .route("/healthz", get(healthz))
        .merge(routes::router())
        .merge(web::router())
        .nest_service("/static", ServeDir::new("static"))
        .nest_service("/uploads", ServeDir::new(UPLOADS_DIR))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state);
}
