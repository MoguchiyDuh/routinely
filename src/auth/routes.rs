use askama::Template;
use axum::Form;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::Html;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::app::AppState;

use super::AuthError;
use super::extract::ApiAuth;
use super::extract::token_from_header_value;
use super::password::PasswordError;
use super::perform_login;
use super::perform_signup;
use super::session;
use super::sync_timezone;

#[derive(Debug, Error)]
pub enum RouteError {
    #[error(transparent)]
    Template(#[from] askama::Error),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Hash(#[from] PasswordError),
    #[error(transparent)]
    Header(#[from] header::InvalidHeaderValue),
}

impl IntoResponse for RouteError {
    fn into_response(self) -> Response {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "internal" })),
        )
            .into_response();
    }
}

#[derive(Template)]
#[template(path = "signup.html")]
struct SignupPage {
    error: Option<String>,
    username: String,
    code: String,
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    error: Option<String>,
    username: String,
}

#[derive(Debug, Deserialize)]
pub struct SignupPayload {
    pub username: Option<String>,
    pub password: Option<String>,
    pub timezone: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginPayload {
    pub username: Option<String>,
    pub password: Option<String>,
    pub timezone: Option<String>,
}

#[derive(Debug, Serialize)]
struct UserBody {
    id: i64,
    username: String,
    timezone: String,
    avatar: serde_json::Value,
}

impl UserBody {
    fn from_user(user: &session::SessionUser, avatar: serde_json::Value) -> Self {
        return Self {
            id: user.id,
            username: user.username.clone(),
            timezone: user.timezone.clone(),
            avatar,
        };
    }
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/signup", get(signup_page).post(signup_submit))
        .route("/login", get(login_page).post(login_submit))
        .route("/logout", post(logout))
        .route("/api/v1/auth/signup", post(api_signup))
        .route("/api/v1/auth/login", post(api_login))
        .route("/api/v1/auth/logout", post(api_logout))
        .route("/api/v1/auth/me", get(api_me))
        .route("/api/v1/me", get(api_me));
}

async fn signup_page() -> Result<Response, RouteError> {
    return html_response(&SignupPage {
        error: None,
        username: String::new(),
        code: String::new(),
    });
}

async fn signup_submit(
    State(state): State<AppState>,
    Form(payload): Form<SignupPayload>,
) -> Result<Response, RouteError> {
    let username = payload.username.clone().unwrap_or_default();
    let code = payload.code.clone().unwrap_or_default();
    let result = perform_signup(
        &state.pool,
        &state.config,
        payload.username,
        payload.password,
        payload.timezone,
        payload.code,
    )
    .await;
    return match result {
        Ok(user) => authed_redirect(&state, &user).await,
        Err(err) => html_response(&SignupPage {
            error: Some(signup_message(&err).to_string()),
            username,
            code,
        }),
    };
}

async fn login_page() -> Result<Response, RouteError> {
    return html_response(&LoginPage {
        error: None,
        username: String::new(),
    });
}

async fn login_submit(
    State(state): State<AppState>,
    Form(payload): Form<LoginPayload>,
) -> Result<Response, RouteError> {
    let username = payload.username.clone().unwrap_or_default();
    let result = perform_login(&state.pool, payload.username, payload.password).await;
    return match result {
        Ok(mut user) => {
            sync_timezone(&state.pool, &mut user, payload.timezone.as_deref()).await?;
            return authed_redirect(&state, &user).await;
        }
        Err(err) => html_response(&LoginPage {
            error: Some(login_message(&err).to_string()),
            username,
        }),
    };
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, RouteError> {
    if let Some(token) = token_from_headers(&headers) {
        session::delete(&state.pool, &token).await?;
    }
    return redirect_with_cookie("/login", &session::clear_cookie());
}

async fn api_signup(
    State(state): State<AppState>,
    Json(payload): Json<SignupPayload>,
) -> Result<Response, RouteError> {
    let result = perform_signup(
        &state.pool,
        &state.config,
        payload.username,
        payload.password,
        payload.timezone,
        payload.code,
    )
    .await;
    return match result {
        Ok(user) => {
            let token = session::create(&state.pool, user.id, state.config.session_ttl).await?;
            let body = user_body(&state, &user).await?;
            let mut response = (StatusCode::OK, Json(body)).into_response();
            set_cookie(
                &mut response,
                &session::build_cookie(&token, state.config.cookie_secure),
            )?;
            return Ok(response);
        }
        Err(err) => Ok(api_error_response(&err)),
    };
}

async fn api_login(
    State(state): State<AppState>,
    Json(payload): Json<LoginPayload>,
) -> Result<Response, RouteError> {
    let result = perform_login(&state.pool, payload.username, payload.password).await;
    return match result {
        Ok(user) => {
            let token = session::create(&state.pool, user.id, state.config.session_ttl).await?;
            let body = user_body(&state, &user).await?;
            let mut response = (StatusCode::OK, Json(body)).into_response();
            set_cookie(
                &mut response,
                &session::build_cookie(&token, state.config.cookie_secure),
            )?;
            return Ok(response);
        }
        Err(err) => Ok(api_error_response(&err)),
    };
}

async fn api_logout(
    State(state): State<AppState>,
    _auth: ApiAuth,
    headers: HeaderMap,
) -> Result<Response, RouteError> {
    if let Some(token) = token_from_headers(&headers) {
        session::delete(&state.pool, &token).await?;
    }
    let mut response = (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response();
    set_cookie(&mut response, &session::clear_cookie())?;
    return Ok(response);
}

async fn api_me(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: ApiAuth,
) -> Result<Json<UserBody>, RouteError> {
    let body = user_body(&state, &auth.0).await?;
    return Ok(Json(body));
}

async fn user_body(state: &AppState, user: &session::SessionUser) -> Result<UserBody, RouteError> {
    let avatar = crate::web::AvatarVm::load(&state.pool, user.id).await?;
    return Ok(UserBody::from_user(
        user,
        crate::web::shell::avatar_json(&avatar),
    ));
}

fn html_response<T: Template>(template: &T) -> Result<Response, RouteError> {
    let html = template.render()?;
    return Ok(Html(html).into_response());
}

async fn authed_redirect(
    state: &AppState,
    user: &session::SessionUser,
) -> Result<Response, RouteError> {
    let token = session::create(&state.pool, user.id, state.config.session_ttl).await?;
    let cookie = session::build_cookie(&token, state.config.cookie_secure);
    return redirect_with_cookie("/", &cookie);
}

fn redirect_with_cookie(location: &'static str, cookie: &str) -> Result<Response, RouteError> {
    let mut response = (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response();
    set_cookie(&mut response, cookie)?;
    return Ok(response);
}

fn set_cookie(response: &mut Response, cookie: &str) -> Result<(), RouteError> {
    let value = HeaderValue::from_str(cookie)?;
    response.headers_mut().append(header::SET_COOKIE, value);
    return Ok(());
}

fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::COOKIE)?;
    return token_from_header_value(value);
}

fn api_error_response(err: &AuthError) -> Response {
    let (status, code) = match err {
        AuthError::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
        AuthError::UsernameTaken => (StatusCode::CONFLICT, "username_taken"),
        AuthError::InvalidUsername => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_username"),
        AuthError::InvalidPassword => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_password"),
        AuthError::InvalidTimezone => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_timezone"),
        AuthError::InvalidCredentials => (StatusCode::UNAUTHORIZED, "invalid_credentials"),
        AuthError::Hash(_) | AuthError::Db(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    };
    return (status, Json(serde_json::json!({ "error": code }))).into_response();
}

fn signup_message(err: &AuthError) -> &'static str {
    return match err {
        AuthError::Forbidden => "invalid signup code",
        AuthError::UsernameTaken => "username is already taken",
        AuthError::InvalidUsername => {
            "username must be 3-32 characters using letters, digits, _ or -"
        }
        AuthError::InvalidPassword => "password must be 8-128 characters",
        AuthError::InvalidTimezone => "unknown timezone",
        _ => "something went wrong",
    };
}

fn login_message(err: &AuthError) -> &'static str {
    return match err {
        AuthError::InvalidCredentials => "invalid username or password",
        _ => "something went wrong",
    };
}
