mod common;

use std::io::Cursor;
use std::net::SocketAddr;
use std::time::Duration;

use chrono::Datelike;
use chrono::Days;
use chrono::NaiveDate;
use chrono::Utc;
use image::DynamicImage;
use image::GenericImageView;
use image::ImageFormat;
use image::ImageReader;
use serde_json::Value;
use sqlx::PgPool;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use routinely::app::AppState;
use routinely::db;
use routinely::score;

const SIGNUP_CODE: &str = "test-code";
const PASSWORD: &str = "password123";
const BOUNDARY: &str = "testboundary";

struct Server {
    addr: SocketAddr,
    pool: PgPool,
    kv: routinely::kv::Kv,
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    headers: String,
    body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<String> {
        for line in self.headers.lines().skip(1) {
            if let Some((key, value)) = line.split_once(':')
                && key.trim().eq_ignore_ascii_case(name)
            {
                return Some(value.trim().to_string());
            }
        }
        return None;
    }

    fn session_token(&self) -> Option<String> {
        let cookie = self.header("set-cookie")?;
        let rest = cookie.strip_prefix("session=")?;
        let token = rest.split(';').next()?;
        if token.is_empty() {
            return None;
        }
        return Some(token.to_string());
    }

    fn json(&self) -> Value {
        return serde_json::from_str(&self.body).expect("response body is not valid JSON");
    }
}

async fn setup(_name: &str) -> Server {
    let config = common::config(SIGNUP_CODE, Duration::from_secs(600)).await;
    let pool = db::init(&config).await.unwrap();
    let kv = common::kv(&config).await;
    let state = AppState {
        pool: pool.clone(),
        kv: kv.clone(),
        config: config.clone(),
    };
    let router = routinely::app::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    return Server { addr, pool, kv };
}

async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> HttpResponse {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n");
    for (name, value) in headers {
        req.push_str(&format!("{name}: {value}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut bytes = req.into_bytes();
    bytes.extend_from_slice(body);
    stream.write_all(&bytes).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    return parse_response(&raw);
}

fn parse_response(raw: &[u8]) -> HttpResponse {
    let text = String::from_utf8_lossy(raw).to_string();
    let (head, body) = match text.split_once("\r\n\r\n") {
        Some((head, body)) => (head.to_string(), body.to_string()),
        None => (text.clone(), String::new()),
    };
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    return HttpResponse {
        status,
        headers: head,
        body,
    };
}

async fn signup_user(server: &Server, username: &str) -> String {
    let body = serde_json::json!({
        "username": username,
        "password": PASSWORD,
        "code": SIGNUP_CODE,
    });
    let response = request(
        server.addr,
        "POST",
        "/api/v1/auth/signup",
        &[("Content-Type", "application/json")],
        body.to_string().as_bytes(),
    )
    .await;
    assert_eq!(response.status, 200, "signup failed: {response:?}");
    return response.session_token().unwrap();
}

async fn login_token(server: &Server, username: &str, password: &str) -> String {
    let body = serde_json::json!({ "username": username, "password": password });
    let response = request(
        server.addr,
        "POST",
        "/api/v1/auth/login",
        &[("Content-Type", "application/json")],
        body.to_string().as_bytes(),
    )
    .await;
    assert_eq!(response.status, 200, "login failed: {response:?}");
    return response.session_token().unwrap();
}

async fn login_status(server: &Server, username: &str, password: &str) -> u16 {
    let body = serde_json::json!({ "username": username, "password": password });
    let response = request(
        server.addr,
        "POST",
        "/api/v1/auth/login",
        &[("Content-Type", "application/json")],
        body.to_string().as_bytes(),
    )
    .await;
    return response.status;
}

async fn json_call(
    server: &Server,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> HttpResponse {
    let cookie = format!("session={token}");
    return request(
        server.addr,
        method,
        path,
        &[
            ("Cookie", cookie.as_str()),
            ("Content-Type", "application/json"),
        ],
        body.to_string().as_bytes(),
    )
    .await;
}

async fn get_page(server: &Server, path: &str, token: &str) -> HttpResponse {
    let cookie = format!("session={token}");
    return request(
        server.addr,
        "GET",
        path,
        &[("Cookie", cookie.as_str())],
        b"",
    )
    .await;
}

async fn post_form(server: &Server, path: &str, token: &str, body: &str) -> HttpResponse {
    let cookie = format!("session={token}");
    return request(
        server.addr,
        "POST",
        path,
        &[
            ("Cookie", cookie.as_str()),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        body.as_bytes(),
    )
    .await;
}

async fn post_multipart(
    server: &Server,
    method: &str,
    path: &str,
    token: &str,
    field: &str,
    content: &[u8],
) -> HttpResponse {
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"photo\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let cookie = format!("session={token}");
    let content_type = format!("multipart/form-data; boundary={BOUNDARY}");
    return request(
        server.addr,
        method,
        path,
        &[
            ("Cookie", cookie.as_str()),
            ("Content-Type", content_type.as_str()),
        ],
        &body,
    )
    .await;
}

async fn user_id_by_name(server: &Server, username: &str) -> i64 {
    let (id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE username = $1")
        .bind(username)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    return id;
}

async fn avatar_columns(
    server: &Server,
    user_id: i64,
) -> (Option<String>, Option<String>, Option<i64>) {
    let row: (Option<String>, Option<String>, Option<i64>) =
        sqlx::query_as("SELECT avatar_path, avatar_icon, avatar_color FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    return row;
}

async fn password_hash(server: &Server, user_id: i64) -> String {
    let (hash,): (String,) = sqlx::query_as("SELECT password_hash FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    return hash;
}

async fn session_count(server: &Server, user_id: i64) -> i64 {
    let count = server.kv.session_count(user_id).await.unwrap();
    return i64::try_from(count).unwrap();
}

fn image_bytes(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
    let img = DynamicImage::new_rgb8(width, height);
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), format).unwrap();
    return out;
}

fn today_utc() -> NaiveDate {
    return Utc::now().date_naive();
}

fn past(offset: u64) -> NaiveDate {
    return today_utc() - Days::new(offset);
}

#[tokio::test]
async fn password_change_ssr_field_errors_and_success() {
    let server = setup("pw-ssr").await;
    let token = signup_user(&server, "pwbob").await;
    let other = login_token(&server, "pwbob", PASSWORD).await;
    assert_ne!(token, other);

    let wrong = post_form(
        &server,
        "/settings/password",
        &token,
        "current_password=wrong-password&new_password=newpassword1&confirm_password=newpassword1",
    )
    .await;
    assert_eq!(wrong.status, 200, "wrong current must re-render: {wrong:?}");
    assert!(wrong.body.contains("Current password is incorrect"));

    let mismatch = post_form(
        &server,
        "/settings/password",
        &token,
        &format!(
            "current_password={PASSWORD}&new_password=newpassword1&confirm_password=different1"
        ),
    )
    .await;
    assert_eq!(mismatch.status, 200);
    assert!(mismatch.body.contains("Passwords do not match"));

    let too_short = post_form(
        &server,
        "/settings/password",
        &token,
        &format!("current_password={PASSWORD}&new_password=short&confirm_password=short"),
    )
    .await;
    assert_eq!(too_short.status, 200);
    assert!(too_short.body.contains("Use 8 to 128 characters"));

    let user_id = user_id_by_name(&server, "pwbob").await;
    let hash_before = password_hash(&server, user_id).await;

    let success = post_form(
        &server,
        "/settings/password",
        &token,
        "current_password=password123&new_password=newpassword1&confirm_password=newpassword1",
    )
    .await;
    assert_eq!(success.status, 303, "success must redirect: {success:?}");
    assert_eq!(success.header("location").as_deref(), Some("/settings"));
    let flash = success.header("set-cookie").unwrap();
    assert!(flash.starts_with("flash="), "flash cookie missing: {flash}");

    let hash_after = password_hash(&server, user_id).await;
    assert_ne!(hash_before, hash_after, "password must be rehashed");

    let me_other = get_page(&server, "/api/v1/me", &other).await;
    assert_eq!(me_other.status, 401, "other session must be revoked");
    let me_current = get_page(&server, "/api/v1/me", &token).await;
    assert_eq!(me_current.status, 200, "current session must stay valid");
    assert_eq!(session_count(&server, user_id).await, 1);

    assert_eq!(login_status(&server, "pwbob", PASSWORD).await, 401);
    assert_eq!(login_status(&server, "pwbob", "newpassword1").await, 200);
}

#[tokio::test]
async fn password_change_api_revokes_other_sessions_keeps_current() {
    let server = setup("pw-api").await;
    let current = signup_user(&server, "pwalice").await;
    let other = login_token(&server, "pwalice", PASSWORD).await;
    assert_ne!(current, other);

    let wrong = json_call(
        &server,
        "PUT",
        "/api/v1/me/password",
        &current,
        serde_json::json!({ "current_password": "wrong-password", "new_password": "newpassword1" }),
    )
    .await;
    assert_eq!(wrong.status, 403, "wrong current must be 403: {wrong:?}");
    assert!(wrong.body.contains("wrong_current_password"));

    let invalid = json_call(
        &server,
        "PUT",
        "/api/v1/me/password",
        &current,
        serde_json::json!({ "current_password": PASSWORD, "new_password": "short" }),
    )
    .await;
    assert_eq!(invalid.status, 422);

    let ok = json_call(
        &server,
        "PUT",
        "/api/v1/me/password",
        &current,
        serde_json::json!({ "current_password": PASSWORD, "new_password": "newpassword1" }),
    )
    .await;
    assert_eq!(ok.status, 204, "success must be 204: {ok:?}");

    let me_current = get_page(&server, "/api/v1/me", &current).await;
    assert_eq!(me_current.status, 200, "current session must stay valid");
    let me_other = get_page(&server, "/api/v1/me", &other).await;
    assert_eq!(me_other.status, 401, "other session must be revoked");
    assert_eq!(login_status(&server, "pwalice", PASSWORD).await, 401);
    assert_eq!(login_status(&server, "pwalice", "newpassword1").await, 200);

    let unauth = request(
        server.addr,
        "PUT",
        "/api/v1/me/password",
        &[("Content-Type", "application/json")],
        serde_json::json!({ "current_password": PASSWORD, "new_password": "whatever123" })
            .to_string()
            .as_bytes(),
    )
    .await;
    assert_eq!(unauth.status, 401);
}

#[tokio::test]
async fn avatar_upload_processes_to_256_png() {
    let server = setup("avatar-upload").await;
    let token = signup_user(&server, "avapng").await;
    let user_id = user_id_by_name(&server, "avapng").await;

    let response = post_multipart(
        &server,
        "POST",
        "/settings/avatar",
        &token,
        "photo",
        &image_bytes(300, 200, ImageFormat::Png),
    )
    .await;
    assert_eq!(response.status, 303, "upload must redirect: {response:?}");
    assert_eq!(response.header("location").as_deref(), Some("/settings"));
    let flash = response.header("set-cookie").unwrap();
    assert!(flash.starts_with("flash="));

    let (path, icon, color) = avatar_columns(&server, user_id).await;
    let path = path.expect("avatar_path must be set");
    assert!(icon.is_none(), "icon must be cleared");
    assert!(color.is_none(), "color must be cleared");
    assert!(path.ends_with(".png"), "stored avatar must be png: {path}");

    let stored = std::fs::read(format!("data/uploads/{path}")).unwrap();
    let reader = ImageReader::new(Cursor::new(&stored))
        .with_guessed_format()
        .unwrap();
    assert_eq!(reader.format(), Some(ImageFormat::Png));
    let decoded = image::load_from_memory(&stored).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (256, 256));

    let remove = post_form(&server, "/settings/avatar/remove", &token, "").await;
    assert_eq!(remove.status, 303);
    let (path, icon, color) = avatar_columns(&server, user_id).await;
    assert!(path.is_none() && icon.is_none() && color.is_none());
}

#[tokio::test]
async fn avatar_upload_accepts_jpeg_webp_and_rejects_junk() {
    let server = setup("avatar-formats").await;
    let token = signup_user(&server, "avajpg").await;
    let user_id = user_id_by_name(&server, "avajpg").await;

    let jpeg = post_multipart(
        &server,
        "POST",
        "/settings/avatar",
        &token,
        "photo",
        &image_bytes(120, 240, ImageFormat::Jpeg),
    )
    .await;
    assert_eq!(jpeg.status, 303, "jpeg upload failed: {jpeg:?}");
    let (path, _, _) = avatar_columns(&server, user_id).await;
    let path = path.unwrap();
    let stored = std::fs::read(format!("data/uploads/{path}")).unwrap();
    assert_eq!(
        image::load_from_memory(&stored).unwrap().dimensions(),
        (256, 256)
    );

    let webp = post_multipart(
        &server,
        "PUT",
        "/api/v1/me/avatar",
        &token,
        "photo",
        &image_bytes(80, 80, ImageFormat::WebP),
    )
    .await;
    assert_eq!(webp.status, 200, "webp upload failed: {webp:?}");
    let payload = webp.json();
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["avatar"]["kind"], "photo");
    assert_eq!(payload["avatar"]["color"], user_id.rem_euclid(10));
    assert!(
        payload["avatar"]["url"]
            .as_str()
            .unwrap()
            .starts_with("/uploads/")
    );
    let (path, icon, _) = avatar_columns(&server, user_id).await;
    assert!(path.unwrap().ends_with(".png"));
    assert!(icon.is_none());

    let junk = post_multipart(
        &server,
        "POST",
        "/settings/avatar",
        &token,
        "photo",
        b"this is not an image at all",
    )
    .await;
    assert_eq!(junk.status, 422, "non-image must be rejected: {junk:?}");
    assert!(junk.body.contains("png, jpeg or webp"));

    let junk_api = post_multipart(
        &server,
        "PUT",
        "/api/v1/me/avatar",
        &token,
        "photo",
        b"\x00\x01\x02\x03",
    )
    .await;
    assert_eq!(junk_api.status, 422);
    assert_eq!(junk_api.json()["error"], "validation");
}

#[tokio::test]
async fn avatar_icon_allowlist_and_remove() {
    let server = setup("avatar-icon").await;
    let token = signup_user(&server, "avaicon").await;
    let user_id = user_id_by_name(&server, "avaicon").await;

    let bad_icon = post_form(
        &server,
        "/settings/avatar/icon",
        &token,
        "icon=dragon&color=3",
    )
    .await;
    assert_eq!(bad_icon.status, 200, "unknown icon must re-render");
    assert!(bad_icon.body.contains("preset icons"));

    let bad_color = post_form(
        &server,
        "/settings/avatar/icon",
        &token,
        "icon=cat&color=10",
    )
    .await;
    assert_eq!(bad_color.status, 200, "color 10 must re-render");
    assert!(bad_color.body.contains("Pick a color"));

    let ok = post_form(&server, "/settings/avatar/icon", &token, "icon=cat&color=3").await;
    assert_eq!(ok.status, 303);
    let (path, icon, color) = avatar_columns(&server, user_id).await;
    assert!(path.is_none(), "photo must be cleared by icon");
    assert_eq!(icon.as_deref(), Some("cat"));
    assert_eq!(color, Some(3));

    let api_bad_icon = json_call(
        &server,
        "PUT",
        "/api/v1/me/avatar/icon",
        &token,
        serde_json::json!({ "icon": "dragon", "color": 3 }),
    )
    .await;
    assert_eq!(api_bad_icon.status, 422);

    let api_bad_color = json_call(
        &server,
        "PUT",
        "/api/v1/me/avatar/icon",
        &token,
        serde_json::json!({ "icon": "cat", "color": 10 }),
    )
    .await;
    assert_eq!(api_bad_color.status, 422);

    let api_icon = json_call(
        &server,
        "PUT",
        "/api/v1/me/avatar/icon",
        &token,
        serde_json::json!({ "icon": "rocket", "color": 7 }),
    )
    .await;
    assert_eq!(api_icon.status, 200);
    assert_eq!(api_icon.json()["avatar"]["kind"], "icon");
    assert_eq!(api_icon.json()["avatar"]["icon"], "rocket");
    assert_eq!(api_icon.json()["avatar"]["color"], 7);

    let me = get_page(&server, "/api/v1/me", &token).await;
    let me_payload = me.json();
    assert_eq!(me_payload["avatar"]["kind"], "icon");
    assert_eq!(me_payload["avatar"]["icon"], "rocket");
    assert_eq!(me_payload["avatar"]["color"], 7);
    assert!(me_payload["avatar"]["url"].is_null());
    assert_eq!(me_payload["id"], user_id);
    assert_eq!(me_payload["username"], "avaicon");

    let settings_page = get_page(&server, "/settings", &token).await;
    assert_eq!(settings_page.status, 200);
    assert!(settings_page.body.contains(r#"value="rocket""#));
    assert!(settings_page.body.contains(r#"/settings/avatar/remove"#));

    let remove_api = request(
        server.addr,
        "DELETE",
        "/api/v1/me/avatar",
        &[("Cookie", format!("session={token}").as_str())],
        b"",
    )
    .await;
    assert_eq!(remove_api.status, 200);
    let (path, icon, color) = avatar_columns(&server, user_id).await;
    assert!(path.is_none() && icon.is_none() && color.is_none());

    let me_after = get_page(&server, "/api/v1/me", &token).await;
    assert_eq!(me_after.json()["avatar"]["kind"], "initials");

    let page_after = get_page(&server, "/settings", &token).await;
    assert!(
        !page_after.body.contains(r#"/settings/avatar/remove"#),
        "remove button must disappear for initials avatar"
    );
}

#[tokio::test]
async fn settings_page_renders_avatar_and_password_cards() {
    let server = setup("settings-page").await;
    let token = signup_user(&server, "setpage").await;

    let page = get_page(&server, "/settings", &token).await;
    assert_eq!(page.status, 200);
    assert!(page.body.contains(r#"action="/settings/avatar""#));
    assert!(
        page.body
            .contains(r#"accept="image/png,image/jpeg,image/webp""#)
    );
    assert!(page.body.contains(r#"name="icon""#));
    assert!(page.body.contains(r#"aria-label="Color 1""#));
    assert!(page.body.contains(r#"aria-label="Color 10""#));
    assert!(page.body.contains(r#"aria-label="cat""#));
    assert!(page.body.contains(r#"aria-label="book-open""#));
    assert!(page.body.contains(r#"name="current_password""#));
    assert!(page.body.contains(r#"name="new_password""#));
    assert!(page.body.contains(r#"name="confirm_password""#));
    assert!(page.body.contains(r#"autocomplete="current-password""#));
    assert!(page.body.contains(r#"autocomplete="new-password""#));
    assert!(page.body.contains(r#"<label for="password-current">"#));
    assert!(page.body.contains(r#"<label for="password-new">"#));
    assert!(page.body.contains(r#"<label for="password-confirm">"#));

    let unauth = request(server.addr, "GET", "/settings", &[], b"").await;
    assert_eq!(unauth.status, 303);
    assert_eq!(unauth.header("location").as_deref(), Some("/login"));
}

#[tokio::test]
async fn heatmap_completed_day_level_4_missed_day_level_1() {
    let server = setup("heatmap").await;
    let token = signup_user(&server, "heater").await;
    let user_id = user_id_by_name(&server, "heater").await;
    let payload = serde_json::json!({
        "title": "heatmap routine",
        "kind": "weekdays",
        "weekdays": [0, 1, 2, 3, 4, 5, 6],
    });
    let created = json_call(&server, "POST", "/api/v1/routines", &token, payload).await;
    assert_eq!(created.status, 200, "routine create failed: {created:?}");
    let routine_id = created.json()["id"].as_i64().unwrap();
    sqlx::query("UPDATE routines SET start_date = $1 WHERE id = $2")
        .bind(past(40).to_string())
        .bind(routine_id)
        .execute(&server.pool)
        .await
        .unwrap();

    let done_day = past(2);
    let missed_day = past(3);
    seed_tick(&server.pool, routine_id, done_day).await;

    let days = score::heatmap(&server.pool, user_id, "UTC").await.unwrap();
    assert_eq!(days.len(), 84, "heatmap must cover 12 weeks");
    let done_cell = days.iter().find(|d| d.date == done_day).unwrap();
    assert_eq!(done_cell.done, 1);
    assert_eq!(done_cell.expected, 1);
    assert_eq!(done_cell.level, 4);
    let missed_cell = days.iter().find(|d| d.date == missed_day).unwrap();
    assert_eq!(missed_cell.done, 0);
    assert_eq!(missed_cell.expected, 1);
    assert_eq!(missed_cell.level, 1);
    let today = today_utc();
    let last = days.last().unwrap();
    assert_eq!(last.date.weekday(), chrono::Weekday::Sun, "heatmap must end on Sunday");
    assert!(last.date >= today && last.date < today + chrono::Days::new(7));
    for cell in &days {
        assert_eq!(cell.future, cell.date > today, "future flag wrong on {}", cell.date);
        if cell.future {
            assert_eq!(cell.level, 0);
            assert_eq!(cell.expected, 0);
        }
    }

    let page = get_page(&server, &format!("/users/{user_id}"), &token).await;
    assert_eq!(page.status, 200);
    assert!(
        page.body
            .contains(&format!(r#"aria-label="{done_day}: 1 of 1""#)),
        "done cell label missing: {}",
        page.body
    );
    assert!(page.body.contains(r#"data-l="4""#));
    assert!(
        page.body
            .contains(&format!(r#"aria-label="{missed_day}: 0 of 1""#))
    );
    assert!(page.body.contains(r#"data-l="1""#));
    assert_eq!(
        page.body.contains("is-future"),
        days.iter().any(|d| d.future),
        "future cells render only when the week has days left"
    );
}

#[test]
fn heatmap_level_boundaries() {
    assert_eq!(score::heatmap_level(0, 0), 0);
    assert_eq!(score::heatmap_level(3, 0), 0);
    assert_eq!(score::heatmap_level(0, 4), 1);
    assert_eq!(score::heatmap_level(1, 4), 2);
    assert_eq!(score::heatmap_level(3, 4), 3);
    assert_eq!(score::heatmap_level(4, 4), 4);
    assert_eq!(score::heatmap_level(5, 4), 4);
    assert_eq!(score::heatmap_level(1, 5), 1);
    assert_eq!(score::heatmap_level(2, 5), 2);
    assert_eq!(score::heatmap_level(4, 5), 3);
}

async fn seed_tick(pool: &PgPool, routine_id: i64, date: NaiveDate) -> i64 {
    let (user_id,): (i64,) = sqlx::query_as("SELECT user_id FROM routines WHERE id = $1")
        .bind(routine_id)
        .fetch_one(pool)
        .await
        .unwrap();
    let routine = routinely::store::routine_for_user(pool, user_id, routine_id)
        .await
        .unwrap()
        .unwrap();
    return routinely::store::tick(pool, &routine, date, None).await.unwrap();
}
