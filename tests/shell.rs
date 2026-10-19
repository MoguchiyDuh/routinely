mod common;

use std::net::SocketAddr;
use std::time::Duration;

use sqlx::PgPool;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use routinely::app::AppState;
use routinely::db;

const SIGNUP_CODE: &str = "test-code";
const PASSWORD: &str = "password123";

struct Server {
    addr: SocketAddr,
    pool: PgPool,
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

    fn set_cookies(&self) -> Vec<String> {
        let mut out = Vec::new();
        for line in self.headers.lines().skip(1) {
            if let Some((key, value)) = line.split_once(':')
                && key.trim().eq_ignore_ascii_case("set-cookie")
            {
                out.push(value.trim().to_string());
            }
        }
        return out;
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
}

async fn setup(_name: &str) -> Server {
    let config = common::config(SIGNUP_CODE, Duration::from_secs(600)).await;
    let pool = db::init(&config).await.unwrap();
    let state = AppState {
        pool: pool.clone(),
        kv: common::kv(&config).await,
        config: config.clone(),
    };
    let router = routinely::app::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    return Server { addr, pool };
}

async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> HttpResponse {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n");
    for (name, value) in headers {
        req.push_str(&format!("{name}: {value}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    req.push_str(body);
    stream.write_all(req.as_bytes()).await.unwrap();
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
        &body.to_string(),
    )
    .await;
    assert_eq!(response.status, 200, "signup failed: {response:?}");
    return response.session_token().unwrap();
}

async fn user_timezone(server: &Server, username: &str) -> String {
    let (tz,): (String,) = sqlx::query_as("SELECT timezone FROM users WHERE username = $1")
        .bind(username)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    return tz;
}

#[tokio::test]
async fn migration_adds_ui_columns_and_index() {
    let server = setup("migration").await;
    for (table, column) in [
        ("routines", "previous_id"),
        ("users", "avatar_path"),
        ("users", "avatar_icon"),
        ("users", "avatar_color"),
        ("users", "onboarding_dismissed_at"),
    ] {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM information_schema.columns WHERE table_name = $1 AND column_name = $2")
                .bind(table)
                .bind(column)
                .fetch_one(&server.pool)
                .await
                .unwrap();
        assert_eq!(count, 1, "{table}.{column} missing");
    }
    let (indexes,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM pg_indexes WHERE indexname = $1")
            .bind("idx_routines_previous")
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(indexes, 1, "idx_routines_previous missing");
}

#[tokio::test]
async fn root_redirects_anonymous_to_login_and_authed_to_day() {
    let server = setup("root").await;
    let anon = request(server.addr, "GET", "/", &[], "").await;
    assert_eq!(anon.status, 303);
    assert_eq!(anon.header("location").as_deref(), Some("/login"));
    let token = signup_user(&server, "rootuser").await;
    let authed = request(
        server.addr,
        "GET",
        "/",
        &[("Cookie", format!("session={token}").as_str())],
        "",
    )
    .await;
    assert_eq!(authed.status, 303);
    assert_eq!(authed.header("location").as_deref(), Some("/day"));
}

#[tokio::test]
async fn tz_cookie_updates_timezone_on_page_request() {
    let server = setup("tz-page").await;
    let token = signup_user(&server, "tzepage").await;
    assert_eq!(user_timezone(&server, "tzepage").await, "UTC");
    let cookie = format!("session={token}; tz=Europe/Berlin");
    let response = request(
        server.addr,
        "GET",
        "/day",
        &[("Cookie", cookie.as_str())],
        "",
    )
    .await;
    assert_eq!(response.status, 200, "day page failed: {response:?}");
    assert_eq!(user_timezone(&server, "tzepage").await, "Europe/Berlin");
}

#[tokio::test]
async fn tz_cookie_invalid_is_ignored() {
    let server = setup("tz-invalid").await;
    let token = signup_user(&server, "tzinvalid").await;
    let cookie = format!("session={token}; tz=Not/AZone");
    let response = request(
        server.addr,
        "GET",
        "/day",
        &[("Cookie", cookie.as_str())],
        "",
    )
    .await;
    assert_eq!(response.status, 200, "day page failed: {response:?}");
    assert_eq!(user_timezone(&server, "tzinvalid").await, "UTC");
}

#[tokio::test]
async fn tz_cookie_on_api_request_is_ignored() {
    let server = setup("tz-api").await;
    let token = signup_user(&server, "tzapi").await;
    let cookie = format!("session={token}; tz=Asia/Tokyo");
    let response = request(
        server.addr,
        "GET",
        "/api/v1/day",
        &[("Cookie", cookie.as_str())],
        "",
    )
    .await;
    assert_eq!(response.status, 200, "api day failed: {response:?}");
    assert_eq!(user_timezone(&server, "tzapi").await, "UTC");
}

#[tokio::test]
async fn login_form_applies_timezone_field() {
    let server = setup("tz-login").await;
    signup_user(&server, "tzlogin").await;
    let body = format!("username=tzlogin&password={PASSWORD}&timezone=Europe%2FBerlin");
    let response = request(
        server.addr,
        "POST",
        "/login",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &body,
    )
    .await;
    assert_eq!(response.status, 303, "form login failed: {response:?}");
    assert_eq!(user_timezone(&server, "tzlogin").await, "Europe/Berlin");

    signup_user(&server, "tzlogin2").await;
    let body = format!("username=tzlogin2&password={PASSWORD}&timezone=Bogus/Zone");
    let response = request(
        server.addr,
        "POST",
        "/login",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &body,
    )
    .await;
    assert_eq!(response.status, 303);
    assert_eq!(user_timezone(&server, "tzlogin2").await, "UTC");
}

#[tokio::test]
async fn ssr_redirect_sets_flash_cookie_and_next_page_clears_it() {
    let server = setup("flash").await;
    let token = signup_user(&server, "flashuser").await;
    let body = "title=Walk&kind=weekdays&monday=1";
    let response = request(
        server.addr,
        "POST",
        "/routines",
        &[
            ("Cookie", format!("session={token}").as_str()),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        body,
    )
    .await;
    assert_eq!(response.status, 303, "routine create failed: {response:?}");
    assert_eq!(response.header("location").as_deref(), Some("/routines"));
    let flash_cookies: Vec<String> = response
        .set_cookies()
        .into_iter()
        .filter(|cookie| cookie.starts_with("flash="))
        .collect();
    assert_eq!(flash_cookies.len(), 1, "flash cookie missing: {response:?}");
    assert!(flash_cookies[0].contains("Routine%20created"));
    assert!(flash_cookies[0].contains("HttpOnly"));
    assert!(flash_cookies[0].contains("SameSite=Lax"));
    assert!(flash_cookies[0].contains("Path=/"));
    assert!(flash_cookies[0].contains("Max-Age=60"));

    let cookie = format!("session={token}; flash=Routine%20created");
    let page = request(
        server.addr,
        "GET",
        "/day",
        &[("Cookie", cookie.as_str())],
        "",
    )
    .await;
    assert_eq!(page.status, 200, "day page failed: {page:?}");
    assert!(!page.body.is_empty(), "day page body must not be empty");
    assert!(
        page.body.contains(r#"class="toast" role="status""#),
        "flash must render as a toast: {}",
        page.body
    );
    assert!(
        page.body.contains("Routine created"),
        "flash text missing from toast: {}",
        page.body
    );
    assert!(
        page.body.contains(r#"data-toast-close"#),
        "toast close button missing: {}",
        page.body
    );
    let cleared: Vec<String> = page
        .set_cookies()
        .into_iter()
        .filter(|cookie| cookie.starts_with("flash="))
        .collect();
    assert_eq!(cleared.len(), 1, "flash clear cookie missing: {page:?}");
    assert!(cleared[0].contains("Max-Age=0"), "cookie: {}", cleared[0]);
}
