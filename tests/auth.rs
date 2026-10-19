mod common;

use std::net::SocketAddr;
use std::time::Duration;

use axum::routing::get;
use sqlx::SqlitePool;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use routinely::app::AppState;
use routinely::auth::extract::PageAuth;
use routinely::auth::routes;
use routinely::auth::session;
use routinely::config::Config;
use routinely::db;

const SIGNUP_CODE: &str = "test-code";
const PASSWORD: &str = "password123";

struct Server {
    addr: SocketAddr,
    pool: SqlitePool,
    config: Config,
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
}

async fn setup(_name: &str) -> Server {
    let config = common::config(SIGNUP_CODE, Duration::from_secs(60)).await;
    return serve(config).await;
}

async fn serve(config: Config) -> Server {
    let pool = db::init(&config).await.unwrap();
    let state = AppState {
        pool: pool.clone(),
        config: config.clone(),
    };
    let router = routinely::app::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    return Server {
        addr,
        pool,
        config,
    };
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

async fn post_json(addr: SocketAddr, path: &str, body: serde_json::Value) -> HttpResponse {
    return request(
        addr,
        "POST",
        path,
        &[("Content-Type", "application/json")],
        &body.to_string(),
    )
    .await;
}

async fn get_with_cookie(addr: SocketAddr, path: &str, token: &str) -> HttpResponse {
    let cookie = format!("session={token}");
    return request(addr, "GET", path, &[("Cookie", cookie.as_str())], "").await;
}

async fn signup_user(server: &Server, username: &str) -> String {
    let body = serde_json::json!({
        "username": username,
        "password": PASSWORD,
        "code": SIGNUP_CODE,
    });
    let response = post_json(server.addr, "/api/v1/auth/signup", body).await;
    assert_eq!(response.status, 200, "signup failed: {response:?}");
    return response.session_token().unwrap();
}

async fn login_token(server: &Server, username: &str, password: &str) -> String {
    let body = serde_json::json!({ "username": username, "password": password });
    let response = post_json(server.addr, "/api/v1/auth/login", body).await;
    assert_eq!(response.status, 200, "login failed: {response:?}");
    return response.session_token().unwrap();
}

async fn session_exists(pool: &SqlitePool, token_hash: &str) -> bool {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT user_id FROM sessions WHERE token_hash = $1 AND expires_at > $2")
            .bind(token_hash)
            .bind(chrono::Utc::now().timestamp())
            .fetch_optional(pool)
            .await
            .unwrap();
    return row.is_some();
}

async fn user_timezones(pool: &SqlitePool, username: &str) -> Vec<String> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT timezone FROM users WHERE username = $1")
        .bind(username)
        .fetch_all(pool)
        .await
        .unwrap();
    return rows.into_iter().map(|(tz,)| tz).collect();
}

#[tokio::test]
async fn signup_with_correct_code_creates_user_with_phc_hash() {
    let server = setup("signup-ok").await;
    let body = serde_json::json!({
        "username": "alice",
        "password": PASSWORD,
        "code": SIGNUP_CODE,
    });
    let response = post_json(server.addr, "/api/v1/auth/signup", body).await;
    assert_eq!(response.status, 200);
    let (id, hash): (i64, String) =
        sqlx::query_as("SELECT id, password_hash FROM users WHERE username = $1")
            .bind("alice")
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert!(hash.starts_with("$argon2id$"), "hash was: {hash}");
    let payload: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(payload["id"], id);
    assert_eq!(payload["username"], "alice");
    assert_eq!(payload["timezone"], "UTC");
}

#[tokio::test]
async fn signup_rejects_bad_code_password_and_username() {
    let server = setup("signup-reject").await;
    let wrong_code = serde_json::json!({
        "username": "walter",
        "password": PASSWORD,
        "code": "not-the-code",
    });
    let response = post_json(server.addr, "/api/v1/auth/signup", wrong_code).await;
    assert_eq!(response.status, 403);

    let missing_code = serde_json::json!({
        "username": "walter",
        "password": PASSWORD,
    });
    let response = post_json(server.addr, "/api/v1/auth/signup", missing_code).await;
    assert_eq!(response.status, 403);

    let short_password = serde_json::json!({
        "username": "walter",
        "password": "short",
        "code": SIGNUP_CODE,
    });
    let response = post_json(server.addr, "/api/v1/auth/signup", short_password).await;
    assert_eq!(response.status, 422);

    let short_username = serde_json::json!({
        "username": "no",
        "password": PASSWORD,
        "code": SIGNUP_CODE,
    });
    let response = post_json(server.addr, "/api/v1/auth/signup", short_username).await;
    assert_eq!(response.status, 422);

    let bad_chars = serde_json::json!({
        "username": "has space",
        "password": PASSWORD,
        "code": SIGNUP_CODE,
    });
    let response = post_json(server.addr, "/api/v1/auth/signup", bad_chars).await;
    assert_eq!(response.status, 422);

    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn duplicate_username_case_insensitive_conflicts() {
    let server = setup("dup-username").await;
    signup_user(&server, "bob").await;
    let body = serde_json::json!({
        "username": "BOB",
        "password": PASSWORD,
        "code": SIGNUP_CODE,
    });
    let response = post_json(server.addr, "/api/v1/auth/signup", body).await;
    assert_eq!(response.status, 409);
    assert!(response.body.contains("username_taken"));
}

#[tokio::test]
async fn login_sets_cookie_and_me_returns_user() {
    let server = setup("login-flow").await;
    signup_user(&server, "carol").await;
    let body = serde_json::json!({ "username": "CAROL", "password": PASSWORD });
    let response = post_json(server.addr, "/api/v1/auth/login", body).await;
    assert_eq!(response.status, 200);
    let cookie = response.header("set-cookie").unwrap();
    assert!(cookie.starts_with("session="));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    assert!(cookie.contains("Path=/"));
    assert!(!cookie.contains("Secure"));
    let token = response.session_token().unwrap();
    let token_hash = session::hash_token(&token);
    assert!(session_exists(&server.pool, &token_hash).await);
    let me = get_with_cookie(server.addr, "/api/v1/auth/me", &token).await;
    assert_eq!(me.status, 200);
    let payload: serde_json::Value = serde_json::from_str(&me.body).unwrap();
    assert_eq!(payload["username"], "carol");
    assert_eq!(payload["id"], 1);
    assert_eq!(payload["timezone"], "UTC");
}

#[tokio::test]
async fn login_rejects_wrong_password_and_unknown_user() {
    let server = setup("login-reject").await;
    signup_user(&server, "dave").await;
    let wrong_password = serde_json::json!({ "username": "dave", "password": "wrong-password" });
    let response = post_json(server.addr, "/api/v1/auth/login", wrong_password).await;
    assert_eq!(response.status, 401);
    let unknown_user = serde_json::json!({ "username": "ghost", "password": PASSWORD });
    let response = post_json(server.addr, "/api/v1/auth/login", unknown_user).await;
    assert_eq!(response.status, 401);
}

#[tokio::test]
async fn logout_invalidates_session() {
    let server = setup("logout").await;
    signup_user(&server, "erin").await;
    let token = login_token(&server, "erin", PASSWORD).await;
    let token_hash = session::hash_token(&token);
    let cookie = format!("session={token}");
    let response = request(
        server.addr,
        "POST",
        "/api/v1/auth/logout",
        &[("Cookie", cookie.as_str())],
        "",
    )
    .await;
    assert_eq!(response.status, 200);
    assert!(!session_exists(&server.pool, &token_hash).await);
    let me = get_with_cookie(server.addr, "/api/v1/auth/me", &token).await;
    assert_eq!(me.status, 401);
    assert_eq!(me.body, r#"{"error":"unauthorized"}"#);
}

#[tokio::test]
async fn expired_session_is_rejected() {
    let server = setup("expired").await;
    signup_user(&server, "frank").await;
    let (user_id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE username = $1")
        .bind("frank")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    let token = session::create(&server.pool, user_id, Duration::from_secs(1))
        .await
        .unwrap();
    let token_hash = session::hash_token(&token);
    let me = get_with_cookie(server.addr, "/api/v1/auth/me", &token).await;
    assert_eq!(me.status, 200);
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let me = get_with_cookie(server.addr, "/api/v1/auth/me", &token).await;
    assert_eq!(me.status, 401);
    assert!(!session_exists(&server.pool, &token_hash).await);
}

#[tokio::test]
async fn page_auth_redirects_to_login() {
    let server = setup("page-auth").await;
    let state = AppState {
        pool: server.pool.clone(),
        config: server.config.clone(),
    };
    let router = routes::router()
        .route("/probe", get(probe))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let probe_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let response = request(probe_addr, "GET", "/probe", &[], "").await;
    assert_eq!(response.status, 303);
    assert_eq!(response.header("location").as_deref(), Some("/login"));
    let token = signup_user(&server, "grace").await;
    let response = get_with_cookie(probe_addr, "/probe", &token).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "grace");
}

#[tokio::test]
async fn timezone_valid_persisted_and_invalid_rejected() {
    let server = setup("timezone").await;
    let valid = serde_json::json!({
        "username": "henry",
        "password": PASSWORD,
        "code": SIGNUP_CODE,
        "timezone": "Europe/Berlin",
    });
    let response = post_json(server.addr, "/api/v1/auth/signup", valid).await;
    assert_eq!(response.status, 200);
    assert_eq!(
        user_timezones(&server.pool, "henry").await,
        vec!["Europe/Berlin"]
    );

    let invalid = serde_json::json!({
        "username": "iris",
        "password": PASSWORD,
        "code": SIGNUP_CODE,
        "timezone": "Not/AZone",
    });
    let response = post_json(server.addr, "/api/v1/auth/signup", invalid).await;
    assert_eq!(response.status, 422);
    assert!(user_timezones(&server.pool, "iris").await.is_empty());
}

#[tokio::test]
async fn signup_form_with_empty_timezone_stores_utc() {
    let server = setup("signup-form-tz").await;
    let body = format!("code={SIGNUP_CODE}&username=judy&password={PASSWORD}&timezone=");
    let response = request(
        server.addr,
        "POST",
        "/signup",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        &body,
    )
    .await;
    assert_eq!(response.status, 303, "form signup failed: {response:?}");
    assert_eq!(user_timezones(&server.pool, "judy").await, vec!["UTC"]);
}

#[tokio::test]
async fn signup_page_uses_hidden_timezone_field() {
    let server = setup("signup-page-tz").await;
    let response = request(server.addr, "GET", "/signup", &[], "").await;
    assert_eq!(response.status, 200);
    assert!(
        response
            .body
            .contains(r#"<input type="hidden" name="timezone" id="timezone-field">"#),
        "hidden timezone field missing: {}",
        response.body
    );
    assert_eq!(
        response.body.matches(r#"name="timezone""#).count(),
        1,
        "timezone input must render exactly once as hidden: {}",
        response.body
    );
    assert!(
        !response.body.contains("timezone <input"),
        "visible timezone input must not render: {}",
        response.body
    );
}

#[tokio::test]
async fn login_page_uses_current_password_autocomplete() {
    let server = setup("login-ac").await;
    let response = request(server.addr, "GET", "/login", &[], "").await;
    assert_eq!(response.status, 200);
    assert!(
        response.body.contains(r#"autocomplete="current-password""#),
        "login page must use current-password autocomplete: {}",
        response.body
    );
}

#[tokio::test]
async fn signup_page_orders_code_field_before_username() {
    let server = setup("signup-order").await;
    let response = request(server.addr, "GET", "/signup", &[], "").await;
    assert_eq!(response.status, 200);
    let code = response
        .body
        .find(r#"name="code""#)
        .expect("signup code field missing");
    let username = response
        .body
        .find(r#"name="username""#)
        .expect("username field missing");
    assert!(
        code < username,
        "signup code field must render before username field: {}",
        response.body
    );
}

async fn probe(auth: PageAuth) -> String {
    return auth.0.username;
}
