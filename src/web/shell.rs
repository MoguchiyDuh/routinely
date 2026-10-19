use axum::extract::Request;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::header;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::response::Redirect;
use axum::response::Response;
use sqlx::SqlitePool;

use crate::app::AppState;
use crate::auth::session;
use crate::cookie;
use crate::punish;

pub const FLASH_COOKIE_NAME: &str = "flash";

const FLASH_MAX_BYTES: usize = 120;
const PALETTE_SIZE: u8 = 10;

#[derive(Clone)]
pub struct AvatarVm {
    pub kind: &'static str,
    pub photo_url: Option<String>,
    pub icon: Option<String>,
    pub initials: String,
    pub color: u8,
}

pub struct Shell {
    pub user_id: i64,
    pub username: String,
    pub avatar: AvatarVm,
    pub pending_count: u64,
    pub active: &'static str,
    pub flash: Option<String>,
}

type AvatarRow = (String, Option<String>, Option<String>, Option<i64>);

impl AvatarVm {
    pub fn for_user(
        user_id: i64,
        username: &str,
        avatar_path: Option<&str>,
        avatar_icon: Option<&str>,
        avatar_color: Option<i64>,
    ) -> AvatarVm {
        let color = match avatar_color {
            Some(value) => u8::try_from(value).unwrap_or(0),
            None => u8::try_from(user_id.rem_euclid(i64::from(PALETTE_SIZE))).unwrap_or(0),
        };
        let initials = initials(username);
        if let Some(path) = avatar_path {
            return AvatarVm {
                kind: "photo",
                photo_url: Some(format!("/uploads/{path}")),
                icon: None,
                initials,
                color,
            };
        }
        if let Some(icon) = avatar_icon {
            return AvatarVm {
                kind: "icon",
                photo_url: None,
                icon: Some(icon.to_string()),
                initials,
                color,
            };
        }
        return AvatarVm {
            kind: "initials",
            photo_url: None,
            icon: None,
            initials,
            color,
        };
    }

    pub async fn load(pool: &SqlitePool, user_id: i64) -> Result<AvatarVm, sqlx::Error> {
        let row: Option<AvatarRow> = sqlx::query_as(
            "SELECT username, avatar_path, avatar_icon, avatar_color FROM users WHERE id = $1",
        )
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
        let Some((username, avatar_path, avatar_icon, avatar_color)) = row else {
            return Ok(AvatarVm::for_user(user_id, "", None, None, None));
        };
        return Ok(AvatarVm::for_user(
            user_id,
            &username,
            avatar_path.as_deref(),
            avatar_icon.as_deref(),
            avatar_color,
        ));
    }
}

impl Shell {
    pub async fn load(
        state: &AppState,
        auth: &session::SessionUser,
        active: &'static str,
        headers: &HeaderMap,
    ) -> Result<Shell, sqlx::Error> {
        let pending_count = punish::pending_count(&state.pool, auth.id).await?;
        let avatar = AvatarVm::load(&state.pool, auth.id).await?;
        let flash = cookie::get(headers, FLASH_COOKIE_NAME)
            .and_then(|raw| cookie::decode(&raw))
            .filter(|message| !message.is_empty());
        return Ok(Shell {
            user_id: auth.id,
            username: auth.username.clone(),
            avatar,
            pending_count,
            active,
            flash,
        });
    }
}

pub fn avatar_json(avatar: &AvatarVm) -> serde_json::Value {
    return serde_json::json!({
        "kind": avatar.kind,
        "url": avatar.photo_url,
        "icon": avatar.icon,
        "color": avatar.color,
    });
}

pub fn flash_cookie(message: &str, secure: bool) -> String {
    let value = cookie::encode(message, FLASH_MAX_BYTES);
    let mut cookie =
        format!("{FLASH_COOKIE_NAME}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age=60");
    if secure {
        cookie.push_str("; Secure");
    }
    return cookie;
}

pub fn flash_clear_cookie() -> String {
    return format!("{FLASH_COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
}

pub fn redirect_with_flash(target: &str, message: &str, secure: bool) -> Response {
    let mut response = Redirect::to(target).into_response();
    if let Ok(value) = HeaderValue::from_str(&flash_cookie(message, secure)) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    return response;
}

pub async fn clear_flash(headers: HeaderMap, request: Request, next: Next) -> Response {
    let had_flash = cookie::get(&headers, FLASH_COOKIE_NAME).is_some();
    let mut response = next.run(request).await;
    if !had_flash || !response.status().is_success() {
        return response;
    }
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    if !is_html {
        return response;
    }
    if let Ok(value) = HeaderValue::from_str(&flash_clear_cookie()) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    return response;
}

fn initials(username: &str) -> String {
    return username.chars().take(2).collect::<String>().to_uppercase();
}
