use askama::Template;
use axum::Form;
use axum::Router;
use axum::extract::Multipart;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use axum::routing::put;
use serde::Deserialize;

use crate::app::AppState;
use crate::auth;
use crate::auth::PasswordChangeError;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::auth::extract::token_from_header_value;
use crate::avatar;
use crate::web::AvatarVm;
use crate::web::WebError;
use crate::web::WebResult;
use crate::web::html;
use crate::web::shell::Shell;

const MAX_AVATAR_BYTES: usize = 10 * 1024 * 1024;

impl From<avatar::AvatarError> for WebError {
    fn from(err: avatar::AvatarError) -> Self {
        return match err {
            avatar::AvatarError::Unsupported => {
                WebError::Validation("avatar must be a png, jpeg or webp image")
            }
            avatar::AvatarError::TooLarge => {
                WebError::Validation("avatar image exceeds decoder limits")
            }
            avatar::AvatarError::Decode => WebError::Validation("could not decode avatar image"),
            avatar::AvatarError::Encode | avatar::AvatarError::Task => WebError::Internal,
            avatar::AvatarError::Io(err) => WebError::Io(err),
            avatar::AvatarError::Db(err) => WebError::Db(err),
        };
    }
}

#[derive(Template)]
#[template(path = "settings.html")]
pub struct SettingsPage {
    pub shell: Shell,
    avatar: AvatarVm,
    icons: Vec<IconOptionVm>,
    colors: Vec<ColorOptionVm>,
    avatar_error: Option<&'static str>,
    icon_error: Option<&'static str>,
    color_error: Option<&'static str>,
    current_error: Option<&'static str>,
    new_error: Option<&'static str>,
    confirm_error: Option<&'static str>,
}

struct IconOptionVm {
    name: String,
    selected: bool,
}

struct ColorOptionVm {
    value: u8,
    label: String,
    selected: bool,
}

#[derive(Default)]
struct SettingsErrors {
    avatar: Option<&'static str>,
    icon: Option<&'static str>,
    color: Option<&'static str>,
    current: Option<&'static str>,
    new: Option<&'static str>,
    confirm: Option<&'static str>,
}

#[derive(Debug, Deserialize)]
struct PasswordForm {
    current_password: Option<String>,
    new_password: Option<String>,
    confirm_password: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IconForm {
    icon: Option<String>,
    color: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PasswordApiPayload {
    current_password: Option<String>,
    new_password: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IconApiPayload {
    icon: Option<String>,
    color: Option<i64>,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/settings", get(settings_view))
        .route("/settings/password", post(password_ssr))
        .route("/settings/avatar", post(avatar_ssr))
        .route("/settings/avatar/icon", post(icon_ssr))
        .route("/settings/avatar/remove", post(remove_ssr))
        .route("/api/v1/me/password", put(api_password))
        .route("/api/v1/me/avatar", put(api_avatar).delete(api_remove))
        .route("/api/v1/me/avatar/icon", put(api_icon));
}

async fn settings_view(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> WebResult {
    let shell = Shell::load(&state, &auth, "settings", &headers).await?;
    let page = build_page(&state, &auth.id, shell, SettingsErrors::default()).await?;
    return Ok(html(&page)?);
}

async fn password_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<PasswordForm>,
) -> WebResult {
    let mut errors = SettingsErrors::default();
    let next = form.new_password.as_deref().unwrap_or("");
    let confirm = form.confirm_password.as_deref().unwrap_or("");
    if confirm != next {
        errors.confirm = Some("Passwords do not match");
    }
    if errors.confirm.is_none() {
        let token = session_token(&headers).ok_or(WebError::NotFound)?;
        match auth::change_password(
            &state.pool,
            auth.id,
            &token,
            form.current_password.as_deref(),
            form.new_password.as_deref(),
        )
        .await
        {
            Ok(()) => {
                return Ok(crate::web::shell::redirect_with_flash(
                    "/settings",
                    "Password changed",
                    state.config.cookie_secure,
                ));
            }
            Err(PasswordChangeError::WrongCurrent) => {
                errors.current = Some("Current password is incorrect");
            }
            Err(PasswordChangeError::InvalidNew) => {
                errors.new = Some("Use 8 to 128 characters");
            }
            Err(err) => {
                return Err(match err {
                    PasswordChangeError::Hash(_) => WebError::Internal,
                    PasswordChangeError::Db(db) => WebError::Db(db),
                    PasswordChangeError::WrongCurrent | PasswordChangeError::InvalidNew => {
                        WebError::Internal
                    }
                });
            }
        }
    }
    let shell = Shell::load(&state, &auth, "settings", &headers).await?;
    let page = build_page(&state, &auth.id, shell, errors).await?;
    return Ok(html(&page)?);
}

async fn avatar_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> WebResult {
    let mut bytes: Option<Vec<u8>> = None;
    while let Some(field) = multipart.next_field().await? {
        if field.name() == Some("photo") {
            let data = field.bytes().await?;
            if data.len() > MAX_AVATAR_BYTES {
                return render_error(
                    &state,
                    &auth,
                    &headers,
                    "photo exceeds 10 MiB",
                    StatusCode::PAYLOAD_TOO_LARGE,
                )
                .await;
            }
            bytes = Some(data.to_vec());
        }
    }
    let Some(bytes) = bytes else {
        return render_error(
            &state,
            &auth,
            &headers,
            "choose a photo to upload",
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    };
    let processed = match avatar::process_photo(bytes).await {
        Ok(processed) => processed,
        Err(avatar::AvatarError::Unsupported | avatar::AvatarError::Decode) => {
            return render_error(
                &state,
                &auth,
                &headers,
                "photo must be a png, jpeg or webp image",
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await;
        }
        Err(avatar::AvatarError::TooLarge) => {
            return render_error(
                &state,
                &auth,
                &headers,
                "image is too large (max 8192x8192)",
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await;
        }
        Err(err) => return Err(err.into()),
    };
    avatar::save_photo(&state.pool, auth.id, processed).await?;
    return Ok(crate::web::shell::redirect_with_flash(
        "/settings",
        "Avatar updated",
        state.config.cookie_secure,
    ));
}

async fn icon_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<IconForm>,
) -> WebResult {
    let mut errors = SettingsErrors::default();
    let icon = form.icon.as_deref().unwrap_or("");
    let color_raw = form.color.as_deref().unwrap_or("");
    let color = color_raw.parse::<i64>().ok();
    if !avatar::is_valid_icon(icon) {
        errors.icon = Some("Pick one of the preset icons");
    }
    match color {
        Some(value) if avatar::is_valid_color(value) => {}
        _ => {
            errors.color = Some("Pick a color");
        }
    }
    if errors.icon.is_some() || errors.color.is_some() {
        let shell = Shell::load(&state, &auth, "settings", &headers).await?;
        let page = build_page(&state, &auth.id, shell, errors).await?;
        return Ok(html(&page)?);
    }
    avatar::set_icon(&state.pool, auth.id, icon, color.unwrap_or(0)).await?;
    return Ok(crate::web::shell::redirect_with_flash(
        "/settings",
        "Avatar updated",
        state.config.cookie_secure,
    ));
}

async fn remove_ssr(PageAuth(auth): PageAuth, State(state): State<AppState>) -> WebResult {
    avatar::clear(&state.pool, auth.id).await?;
    return Ok(crate::web::shell::redirect_with_flash(
        "/settings",
        "Avatar removed",
        state.config.cookie_secure,
    ));
}

async fn api_password(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(payload): axum::Json<PasswordApiPayload>,
) -> Result<Response, WebError> {
    let token = session_token(&headers).ok_or(WebError::NotFound)?;
    return match auth::change_password(
        &state.pool,
        auth.id,
        &token,
        payload.current_password.as_deref(),
        payload.new_password.as_deref(),
    )
    .await
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(PasswordChangeError::WrongCurrent) => Ok((
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({ "error": "wrong_current_password" })),
        )
            .into_response()),
        Err(PasswordChangeError::InvalidNew) => Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            axum::Json(serde_json::json!({ "error": "invalid_password" })),
        )
            .into_response()),
        Err(PasswordChangeError::Hash(_)) => Err(WebError::Internal),
        Err(PasswordChangeError::Db(err)) => Err(WebError::Db(err)),
    };
}

async fn api_avatar(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Response, WebError> {
    let mut bytes: Option<Vec<u8>> = None;
    while let Some(field) = multipart.next_field().await? {
        if field.name() == Some("photo") {
            let data = field.bytes().await?;
            if data.len() > MAX_AVATAR_BYTES {
                return Err(WebError::Validation("avatar exceeds 10 MiB"));
            }
            bytes = Some(data.to_vec());
        }
    }
    let Some(bytes) = bytes else {
        return Err(WebError::Validation("photo field is required"));
    };
    let processed = avatar::process_photo(bytes).await?;
    avatar::save_photo(&state.pool, auth.id, processed).await?;
    return avatar_ok(&state, auth.id).await;
}

async fn api_icon(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    axum::Json(payload): axum::Json<IconApiPayload>,
) -> Result<Response, WebError> {
    let Some(icon) = payload.icon.as_deref() else {
        return Err(WebError::Validation("icon is required"));
    };
    let Some(color) = payload.color else {
        return Err(WebError::Validation("color is required"));
    };
    if !avatar::is_valid_icon(icon) {
        return Err(WebError::Validation("unknown icon"));
    }
    if !avatar::is_valid_color(color) {
        return Err(WebError::Validation("invalid color"));
    }
    avatar::set_icon(&state.pool, auth.id, icon, color).await?;
    return avatar_ok(&state, auth.id).await;
}

async fn api_remove(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
) -> Result<Response, WebError> {
    avatar::clear(&state.pool, auth.id).await?;
    return Ok((
        StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true })),
    )
        .into_response());
}

async fn avatar_ok(state: &AppState, user_id: i64) -> Result<Response, WebError> {
    let vm = AvatarVm::load(&state.pool, user_id).await?;
    return Ok((
        StatusCode::OK,
        axum::Json(serde_json::json!({
            "ok": true,
            "avatar": crate::web::shell::avatar_json(&vm),
        })),
    )
        .into_response());
}

async fn render_error(
    state: &AppState,
    auth: &crate::auth::session::SessionUser,
    headers: &HeaderMap,
    message: &'static str,
    status: StatusCode,
) -> WebResult {
    let errors = SettingsErrors {
        avatar: Some(message),
        ..SettingsErrors::default()
    };
    let shell = Shell::load(state, auth, "settings", headers).await?;
    let page = build_page(state, &auth.id, shell, errors).await?;
    let body = page.render()?;
    return Ok((status, axum::response::Html(body)).into_response());
}

async fn build_page(
    state: &AppState,
    user_id: &i64,
    shell: Shell,
    errors: SettingsErrors,
) -> Result<SettingsPage, sqlx::Error> {
    let avatar = AvatarVm::load(&state.pool, *user_id).await?;
    let mut icons = Vec::new();
    for name in avatar::ICONS {
        icons.push(IconOptionVm {
            name: name.to_string(),
            selected: avatar.kind == "icon" && avatar.icon.as_deref() == Some(name),
        });
    }
    let mut colors = Vec::new();
    for value in 0..avatar::PALETTE_SIZE {
        colors.push(ColorOptionVm {
            value,
            label: format!("Color {}", value + 1),
            selected: avatar.color == value,
        });
    }
    return Ok(SettingsPage {
        shell,
        avatar,
        icons,
        colors,
        avatar_error: errors.avatar,
        icon_error: errors.icon,
        color_error: errors.color,
        current_error: errors.current,
        new_error: errors.new,
        confirm_error: errors.confirm,
    });
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::COOKIE)?;
    return token_from_header_value(value);
}
