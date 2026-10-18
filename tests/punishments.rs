use std::net::SocketAddr;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use chrono::Datelike;
use chrono::Days;
use chrono::NaiveDate;
use chrono::Utc;
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use routinely::app::AppState;
use routinely::config::Config;
use routinely::db;

const SIGNUP_CODE: &str = "test-code";
const PASSWORD: &str = "password123";

struct Server {
    addr: SocketAddr,
    pool: SqlitePool,
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

async fn setup(name: &str) -> Server {
    let unique = format!(
        "routinely-punish-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let dir = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&dir).unwrap();
    let config = Config {
        db_path: dir.join("punish.db"),
        bind: "127.0.0.1:0".parse().unwrap(),
        signup_code: SIGNUP_CODE.to_string(),
        session_ttl: Duration::from_secs(600),
        cookie_secure: false,
    };
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
    return Server { addr, pool };
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

async fn user_id(server: &Server, token: &str) -> i64 {
    let response = json_call(
        server,
        "GET",
        "/api/v1/auth/me",
        token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status, 200, "me failed: {response:?}");
    return response.json()["id"].as_i64().unwrap();
}

async fn create_routine(server: &Server, token: &str, payload: Value) -> i64 {
    let response = json_call(server, "POST", "/api/v1/routines", token, payload).await;
    assert_eq!(response.status, 200, "routine create failed: {response:?}");
    return response.json()["id"].as_i64().unwrap();
}

async fn count_scalar(server: &Server, sql: &'static str) -> i64 {
    let (value,): (i64,) = sqlx::query_as(sql).fetch_one(&server.pool).await.unwrap();
    return value;
}

async fn scalar_string(server: &Server, sql: &'static str) -> Option<String> {
    let (value,): (Option<String>,) = sqlx::query_as(sql).fetch_one(&server.pool).await.unwrap();
    return value;
}

async fn trigger_eval(server: &Server, token: &str) {
    sqlx::query("UPDATE users SET last_evaluated_day = NULL")
        .execute(&server.pool)
        .await
        .unwrap();
    let response = json_call(server, "GET", "/api/v1/day", token, serde_json::json!({})).await;
    assert_eq!(response.status, 200, "eval trigger failed: {response:?}");
}

async fn catalog_ids(server: &Server) -> Vec<i64> {
    let rows: Vec<(i64,)> = sqlx::query_as("SELECT id FROM punishment_catalog ORDER BY id")
        .fetch_all(&server.pool)
        .await
        .unwrap();
    return rows.into_iter().map(|(id,)| id).collect();
}

async fn backdate_routine_start(server: &Server, routine_id: i64, date: NaiveDate) {
    sqlx::query("UPDATE routines SET start_date = ? WHERE id = ?")
        .bind(date.to_string())
        .bind(routine_id)
        .execute(&server.pool)
        .await
        .unwrap();
}

async fn all_weekdays_routine(server: &Server, token: &str) -> i64 {
    return create_routine(
        server,
        token,
        serde_json::json!({
            "title": "daily drill",
            "kind": "weekdays",
            "weekdays": [0, 1, 2, 3, 4, 5, 6],
        }),
    )
    .await;
}

async fn insert_past_occurrence(server: &Server, routine_id: i64, due: NaiveDate) -> i64 {
    sqlx::query(
        "INSERT INTO occurrences (routine_id, due_date, period_start, created_at) VALUES (?, ?, NULL, ?)",
    )
    .bind(routine_id)
    .bind(due.to_string())
    .bind(Utc::now().to_rfc3339())
    .execute(&server.pool)
    .await
    .unwrap();
    let (id,): (i64,) =
        sqlx::query_as("SELECT id FROM occurrences WHERE routine_id = ? AND due_date = ?")
            .bind(routine_id)
            .bind(due.to_string())
            .fetch_one(&server.pool)
            .await
            .unwrap();
    return id;
}

fn today_utc() -> NaiveDate {
    return Utc::now().date_naive();
}

fn week_monday(today: NaiveDate) -> NaiveDate {
    let offset = today.weekday().num_days_from_monday();
    return today - Days::new(u64::from(offset));
}

#[tokio::test]
async fn first_signup_seeds_eight_defaults_and_second_does_not_duplicate() {
    let server = setup("seeds").await;
    let alice = signup_user(&server, "alice").await;
    let alice_id = user_id(&server, &alice).await;
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM punishment_catalog")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(count, 8, "first signup must seed exactly 8 defaults");
    let titles: Vec<(String,)> = sqlx::query_as("SELECT title FROM punishment_catalog ORDER BY id")
        .fetch_all(&server.pool)
        .await
        .unwrap();
    let mut names: Vec<String> = titles.into_iter().map(|(t,)| t).collect();
    names.sort();
    let mut expected: Vec<String> = [
        "20 pushups",
        "50 jumping jacks",
        "15 min walk",
        "wash the dishes",
        "clean one room",
        "30 min reading",
        "no sugar tomorrow",
        "10 squats",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    expected.sort();
    assert_eq!(names, expected, "seed titles must match the defaults");
    let (foreign,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM punishment_catalog WHERE created_by <> ?")
            .bind(alice_id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(foreign, 0, "seeds must be created_by the signup user");
    let (inactive,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM punishment_catalog WHERE active <> 1")
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(inactive, 0, "seeds must be active");
    let _bob = signup_user(&server, "bob").await;
    let (after,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM punishment_catalog")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(after, 8, "second signup must not duplicate seeds");
}

#[tokio::test]
async fn weekdays_miss_creates_single_pending_punishment_and_is_idempotent() {
    let server = setup("weekday-miss").await;
    let token = signup_user(&server, "carol").await;
    let id = all_weekdays_routine(&server, &token).await;
    let yesterday = today_utc() - Days::new(1);
    backdate_routine_start(&server, id, yesterday).await;
    trigger_eval(&server, &token).await;
    let (occurrence,): (i64,) =
        sqlx::query_as("SELECT id FROM occurrences WHERE routine_id = ? AND due_date = ?")
            .bind(id)
            .bind(yesterday.to_string())
            .fetch_one(&server.pool)
            .await
            .unwrap();
    let row: Option<(String, String, Option<i64>)> = sqlx::query_as(
        "SELECT status, source, catalog_id FROM punishments WHERE occurrence_id = ?",
    )
    .bind(occurrence)
    .fetch_optional(&server.pool)
    .await
    .unwrap();
    let Some((status, source, catalog_id)) = row else {
        panic!("no punishment linked to missed occurrence {occurrence}");
    };
    assert_eq!(status, "pending");
    assert_eq!(source, "auto");
    assert!(catalog_id.is_some(), "auto draw must carry a catalog item");
    let total = count_scalar(&server, "SELECT COUNT(*) FROM punishments").await;
    assert_eq!(total, 1, "only yesterday's occurrence must be punished");
    trigger_eval(&server, &token).await;
    let after = count_scalar(&server, "SELECT COUNT(*) FROM punishments").await;
    assert_eq!(after, 1, "re-eval must not duplicate the punishment");
}

#[tokio::test]
async fn closed_count_period_miss_records_marker_and_punishment_idempotently() {
    let server = setup("count-miss").await;
    let token = signup_user(&server, "dave").await;
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "water plants",
            "kind": "count",
            "per_period": 2,
            "period": "week",
        }),
    )
    .await;
    let prev_monday = week_monday(today_utc()) - Days::new(7);
    backdate_routine_start(&server, id, prev_monday).await;
    seed_tick(&server.pool, id, prev_monday).await;
    trigger_eval(&server, &token).await;
    let punishments: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM punishments WHERE occurrence_id IS NULL AND source = 'auto' AND status = 'pending'",
    )
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        punishments.0, 1,
        "closed under-drawn period must punish once"
    );
    let (markers,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM period_misses WHERE routine_id = ? AND period_start = ?",
    )
    .bind(id)
    .bind(prev_monday.to_string())
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(markers, 1, "period_misses row must exist");
    trigger_eval(&server, &token).await;
    let after = count_scalar(
        &server,
        "SELECT COUNT(*) FROM punishments WHERE occurrence_id IS NULL AND source = 'auto'",
    )
    .await;
    assert_eq!(after, 1, "re-eval must not duplicate count punishment");
    let (markers_after,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM period_misses")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(markers_after, 1, "re-eval must not duplicate marker");
}

#[tokio::test]
async fn cap_limits_auto_draws_to_three_per_user_day() {
    let server = setup("cap").await;
    let token = signup_user(&server, "erin").await;
    for _ in 0..4 {
        let id = all_weekdays_routine(&server, &token).await;
        insert_past_occurrence(&server, id, today_utc() - Days::new(1)).await;
    }
    trigger_eval(&server, &token).await;
    let total = count_scalar(&server, "SELECT COUNT(*) FROM punishments").await;
    assert_eq!(total, 4, "all four misses must be recorded");
    let drawn = count_scalar(
        &server,
        "SELECT COUNT(*) FROM punishments WHERE catalog_id IS NOT NULL",
    )
    .await;
    assert_eq!(drawn, 3, "cap must limit draws to 3");
    let undrawn = count_scalar(
        &server,
        "SELECT COUNT(*) FROM punishments WHERE catalog_id IS NULL AND source = 'auto'",
    )
    .await;
    assert_eq!(undrawn, 1, "overflow miss must be a marker row only");
}

#[tokio::test]
async fn draw_uses_only_active_catalog_items() {
    let server = setup("draw-active").await;
    let token = signup_user(&server, "frank").await;
    let ids = catalog_ids(&server).await;
    assert_eq!(ids.len(), 8);
    for id in &ids[1..] {
        let response = json_call(
            &server,
            "POST",
            &format!("/api/v1/catalog/{id}/deactivate"),
            &token,
            serde_json::json!({}),
        )
        .await;
        assert_eq!(response.status, 200, "deactivate failed: {response:?}");
    }
    let id = all_weekdays_routine(&server, &token).await;
    let yesterday = today_utc() - Days::new(1);
    backdate_routine_start(&server, id, yesterday).await;
    trigger_eval(&server, &token).await;
    let row: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT catalog_id FROM punishments WHERE source = 'auto'")
            .fetch_optional(&server.pool)
            .await
            .unwrap();
    let Some((catalog_id,)) = row else {
        panic!("expected one auto punishment after eval");
    };
    assert_eq!(
        catalog_id,
        Some(ids[0]),
        "draw must select the only active item"
    );
}

#[tokio::test]
async fn clear_waive_conflict_and_foreign_404() {
    let server = setup("clear-waive").await;
    let alice = signup_user(&server, "grace").await;
    let bob = signup_user(&server, "heidi").await;
    let alice_id = user_id(&server, &alice).await;
    let assign = json_call(
        &server,
        "POST",
        &format!("/api/v1/users/{alice_id}/punishments"),
        &alice,
        serde_json::json!({ "custom_title": "scrub the floor" }),
    )
    .await;
    assert_eq!(assign.status, 200, "assign failed: {assign:?}");
    let p1 = assign.json()["id"].as_i64().unwrap();
    let assign2 = json_call(
        &server,
        "POST",
        &format!("/api/v1/users/{alice_id}/punishments"),
        &alice,
        serde_json::json!({ "custom_title": "second chore" }),
    )
    .await;
    let p2 = assign2.json()["id"].as_i64().unwrap();
    let assign3 = json_call(
        &server,
        "POST",
        &format!("/api/v1/users/{alice_id}/punishments"),
        &alice,
        serde_json::json!({ "custom_title": "third chore" }),
    )
    .await;
    let p3 = assign3.json()["id"].as_i64().unwrap();
    let clear = json_call(
        &server,
        "POST",
        &format!("/api/v1/punishments/{p1}/clear"),
        &alice,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(clear.status, 200, "clear failed: {clear:?}");
    let (status, cleared_at): (String, Option<String>) =
        sqlx::query_as("SELECT status, cleared_at FROM punishments WHERE id = ?")
            .bind(p1)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(status, "cleared");
    assert!(cleared_at.is_some(), "clear must set cleared_at");
    let reclear = json_call(
        &server,
        "POST",
        &format!("/api/v1/punishments/{p1}/clear"),
        &alice,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(reclear.status, 409, "clearing non-pending must 409");
    let rewaive = json_call(
        &server,
        "POST",
        &format!("/api/v1/punishments/{p1}/waive"),
        &alice,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(rewaive.status, 409, "waiving non-pending must 409");
    let waive = json_call(
        &server,
        "POST",
        &format!("/api/v1/punishments/{p2}/waive"),
        &alice,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(waive.status, 200, "waive failed: {waive:?}");
    let (status, cleared_at): (String, Option<String>) =
        sqlx::query_as("SELECT status, cleared_at FROM punishments WHERE id = ?")
            .bind(p2)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(status, "waived");
    assert!(cleared_at.is_some(), "waive must set cleared_at");
    let foreign = json_call(
        &server,
        "POST",
        &format!("/api/v1/punishments/{p3}/clear"),
        &bob,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(foreign.status, 404, "mutating foreign punishment must 404");
    let still: (String,) = sqlx::query_as("SELECT status FROM punishments WHERE id = ?")
        .bind(p3)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(still.0, "pending");
    let history = json_call(
        &server,
        "GET",
        "/api/v1/punishments?status=history",
        &alice,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(history.status, 422, "history is not a valid JSON status");
    let all = json_call(
        &server,
        "GET",
        "/api/v1/punishments?status=cleared",
        &alice,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(all.status, 200);
    assert_eq!(all.json().as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn manual_assign_by_catalog_and_custom_title_bypasses_cap() {
    let server = setup("manual-assign").await;
    let token = signup_user(&server, "ivan").await;
    let target = user_id(&server, &token).await;
    for _ in 0..4 {
        let routine = all_weekdays_routine(&server, &token).await;
        insert_past_occurrence(&server, routine, today_utc() - Days::new(1)).await;
    }
    trigger_eval(&server, &token).await;
    let drawn = count_scalar(
        &server,
        "SELECT COUNT(*) FROM punishments WHERE source = 'auto' AND catalog_id IS NOT NULL",
    )
    .await;
    assert_eq!(drawn, 3, "cap must be saturated first");
    let ids = catalog_ids(&server).await;
    let by_catalog = json_call(
        &server,
        "POST",
        &format!("/api/v1/users/{target}/punishments"),
        &token,
        serde_json::json!({ "catalog_id": ids[0] }),
    )
    .await;
    assert_eq!(
        by_catalog.status, 200,
        "manual assign failed: {by_catalog:?}"
    );
    let by_title = json_call(
        &server,
        "POST",
        &format!("/api/v1/users/{target}/punishments"),
        &token,
        serde_json::json!({ "custom_title": "write an apology" }),
    )
    .await;
    assert_eq!(by_title.status, 200, "custom assign failed: {by_title:?}");
    let (manual,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM punishments WHERE source = 'manual' AND status = 'pending' AND occurrence_id IS NULL",
    )
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(manual, 2, "both manual assignments must exist despite cap");
    let auto_total = count_scalar(
        &server,
        "SELECT COUNT(*) FROM punishments WHERE source = 'auto'",
    )
    .await;
    assert_eq!(auto_total, 4, "cap behaviour must be unchanged");
    let both = json_call(
        &server,
        "POST",
        &format!("/api/v1/users/{target}/punishments"),
        &token,
        serde_json::json!({ "catalog_id": ids[0], "custom_title": "both" }),
    )
    .await;
    assert_eq!(both.status, 422, "both fields must 422");
    let neither = json_call(
        &server,
        "POST",
        &format!("/api/v1/users/{target}/punishments"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(neither.status, 422, "neither field must 422");
    let ghost = json_call(
        &server,
        "POST",
        "/api/v1/users/99999/punishments",
        &token,
        serde_json::json!({ "custom_title": "ghost" }),
    )
    .await;
    assert_eq!(ghost.status, 404, "unknown target user must 404");
}

#[tokio::test]
async fn catalog_add_deactivate_activate_roundtrip_and_validation() {
    let server = setup("catalog-crud").await;
    let token = signup_user(&server, "judy").await;
    let created = json_call(
        &server,
        "POST",
        "/api/v1/catalog",
        &token,
        serde_json::json!({ "title": "burpees", "description": "10 reps" }),
    )
    .await;
    assert_eq!(created.status, 200, "catalog add failed: {created:?}");
    let id = created.json()["id"].as_i64().unwrap();
    let list = json_call(
        &server,
        "GET",
        "/api/v1/catalog",
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(list.status, 200);
    let items = list.json().as_array().unwrap().clone();
    let mine = items
        .into_iter()
        .find(|item| item["id"].as_i64() == Some(id))
        .expect("new item in catalog list");
    assert_eq!(mine["title"], "burpees");
    assert_eq!(mine["active"], true);
    let off = json_call(
        &server,
        "POST",
        &format!("/api/v1/catalog/{id}/deactivate"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(off.status, 200);
    let (active,): (i64,) = sqlx::query_as("SELECT active FROM punishment_catalog WHERE id = ?")
        .bind(id)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(active, 0);
    let on = json_call(
        &server,
        "POST",
        &format!("/api/v1/catalog/{id}/activate"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(on.status, 200);
    let (active_again,): (i64,) =
        sqlx::query_as("SELECT active FROM punishment_catalog WHERE id = ?")
            .bind(id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(active_again, 1);
    let empty = json_call(
        &server,
        "POST",
        "/api/v1/catalog",
        &token,
        serde_json::json!({ "title": "" }),
    )
    .await;
    assert_eq!(empty.status, 422, "empty title must 422");
    let blank = json_call(
        &server,
        "POST",
        "/api/v1/catalog",
        &token,
        serde_json::json!({ "title": "   " }),
    )
    .await;
    assert_eq!(blank.status, 422, "blank title must 422");
    let too_long = json_call(
        &server,
        "POST",
        "/api/v1/catalog",
        &token,
        serde_json::json!({ "title": "x".repeat(201) }),
    )
    .await;
    assert_eq!(too_long.status, 422, "title over 200 chars must 422");
    let missing = json_call(
        &server,
        "POST",
        "/api/v1/catalog/99999/deactivate",
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(missing.status, 404, "deactivating unknown item must 404");
}

#[tokio::test]
async fn eval_hook_fires_once_per_user_day() {
    let server = setup("eval-once").await;
    let token = signup_user(&server, "ken").await;
    let first = json_call(&server, "GET", "/api/v1/day", &token, serde_json::json!({})).await;
    assert_eq!(first.status, 200);
    let today = today_utc().to_string();
    let stored = scalar_string(
        &server,
        "SELECT last_evaluated_day FROM users WHERE username = 'ken'",
    )
    .await;
    assert_eq!(stored.as_deref(), Some(today.as_str()));
    let routine = all_weekdays_routine(&server, &token).await;
    let yesterday = today_utc() - Days::new(1);
    backdate_routine_start(&server, routine, yesterday).await;
    insert_past_occurrence(&server, routine, yesterday).await;
    let second = json_call(&server, "GET", "/api/v1/day", &token, serde_json::json!({})).await;
    assert_eq!(second.status, 200);
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM punishments")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "same-day repeat request must skip eval work entirely"
    );
    trigger_eval(&server, &token).await;
    let (after,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM punishments")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(after, 1, "forced re-eval must punish the new miss");
}

#[tokio::test]
async fn punishment_routes_require_auth() {
    let server = setup("unauth").await;
    let page = request(server.addr, "GET", "/punishments", &[], b"").await;
    assert_eq!(page.status, 303, "SSR queue must redirect unauth users");
    assert_eq!(page.header("location").as_deref(), Some("/login"));
    let assign_pick = request(server.addr, "POST", "/punishments/assign", &[], b"").await;
    assert_eq!(
        assign_pick.status, 303,
        "SSR assign must redirect unauth users"
    );
    assert_eq!(assign_pick.header("location").as_deref(), Some("/login"));
    let list = request(server.addr, "GET", "/api/v1/punishments", &[], b"").await;
    assert_eq!(list.status, 401);
    let catalog = request(server.addr, "GET", "/api/v1/catalog", &[], b"").await;
    assert_eq!(catalog.status, 401);
    let clear = request(
        server.addr,
        "POST",
        "/api/v1/punishments/1/clear",
        &[("Content-Type", "application/json")],
        b"{}",
    )
    .await;
    assert_eq!(clear.status, 401);
    let assign = request(
        server.addr,
        "POST",
        "/api/v1/users/1/punishments",
        &[("Content-Type", "application/json")],
        b"{\"custom_title\":\"nope\"}",
    )
    .await;
    assert_eq!(assign.status, 401);
    let add = request(
        server.addr,
        "POST",
        "/api/v1/catalog",
        &[("Content-Type", "application/json")],
        b"{\"title\":\"nope\"}",
    )
    .await;
    assert_eq!(add.status, 401);
    let deactivate = request(
        server.addr,
        "POST",
        "/api/v1/catalog/1/deactivate",
        &[],
        b"",
    )
    .await;
    assert_eq!(deactivate.status, 401);
    let activate = request(server.addr, "POST", "/api/v1/catalog/1/activate", &[], b"").await;
    assert_eq!(activate.status, 401);
}

#[tokio::test]
async fn punishments_page_renders_tabs_queue_and_catalog() {
    let server = setup("page").await;
    let token = signup_user(&server, "lena").await;
    let target = user_id(&server, &token).await;
    let assign = json_call(
        &server,
        "POST",
        &format!("/api/v1/users/{target}/punishments"),
        &token,
        serde_json::json!({ "custom_title": "tidy the desk" }),
    )
    .await;
    assert_eq!(assign.status, 200);
    let cookie = format!("session={token}");
    let page = request(
        server.addr,
        "GET",
        "/punishments",
        &[("Cookie", cookie.as_str())],
        b"",
    )
    .await;
    assert_eq!(page.status, 200, "punishments page failed: {page:?}");
    assert!(
        page.body.contains("tidy the desk"),
        "queue must render: {}",
        page.body
    );
    assert!(
        page.body
            .contains(r#"href="/punishments?tab=mine" aria-current="page""#),
        "mine tab must be active: {}",
        page.body
    );
    assert!(
        page.body.contains(r#"class="nav-badge""#),
        "pending badge must render: {}",
        page.body
    );
    let catalog = request(
        server.addr,
        "GET",
        "/punishments?tab=catalog",
        &[("Cookie", cookie.as_str())],
        b"",
    )
    .await;
    assert_eq!(catalog.status, 200, "catalog tab failed: {catalog:?}");
    assert!(
        catalog
            .body
            .contains(r#"href="/punishments?tab=catalog" aria-current="page""#),
        "catalog tab must be active: {}",
        catalog.body
    );
    assert!(
        catalog.body.contains("20 pushups"),
        "seeded items must render: {}",
        catalog.body
    );
    let id = assign.json()["id"].as_i64().unwrap();
    let clear = request(
        server.addr,
        "POST",
        &format!("/punishments/{id}/clear"),
        &[
            ("Cookie", cookie.as_str()),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        b"tab=mine",
    )
    .await;
    assert_eq!(clear.status, 303, "SSR clear must redirect");
    assert_eq!(
        clear.header("location").as_deref(),
        Some("/punishments?tab=mine"),
        "SSR clear must land back on the mine tab"
    );
}

#[tokio::test]
async fn tab_param_selects_tab_content() {
    let server = setup("tabs").await;
    let alice = signup_user(&server, "tabalice").await;
    let alice_id = user_id(&server, &alice).await;
    let bob = signup_user(&server, "tabbob").await;
    let bob_id = user_id(&server, &bob).await;
    let assign_bob = json_call(
        &server,
        "POST",
        &format!("/api/v1/users/{bob_id}/punishments"),
        &alice,
        serde_json::json!({ "custom_title": "bob chore" }),
    )
    .await;
    assert_eq!(
        assign_bob.status, 200,
        "assign to bob failed: {assign_bob:?}"
    );
    let assign_self = json_call(
        &server,
        "POST",
        &format!("/api/v1/users/{alice_id}/punishments"),
        &alice,
        serde_json::json!({ "custom_title": "alice chore" }),
    )
    .await;
    assert_eq!(
        assign_self.status, 200,
        "assign to self failed: {assign_self:?}"
    );
    let cookie = format!("session={alice}");
    let mine = request(
        server.addr,
        "GET",
        "/punishments?tab=mine",
        &[("Cookie", cookie.as_str())],
        b"",
    )
    .await;
    assert_eq!(mine.status, 200);
    assert!(
        mine.body.contains("alice chore"),
        "mine tab must show own pending: {}",
        mine.body
    );
    assert!(
        !mine.body.contains("bob chore"),
        "mine tab must not show other users' pending: {}",
        mine.body
    );
    let everyone = request(
        server.addr,
        "GET",
        "/punishments?tab=everyone",
        &[("Cookie", cookie.as_str())],
        b"",
    )
    .await;
    assert_eq!(everyone.status, 200);
    assert!(
        everyone.body.contains("tabbob"),
        "everyone tab must show usernames: {}",
        everyone.body
    );
    assert!(
        everyone.body.contains("bob chore"),
        "everyone tab must show other users' pending: {}",
        everyone.body
    );
    assert!(
        everyone.body.contains("alice chore"),
        "everyone tab must show own pending group: {}",
        everyone.body
    );
    assert!(
        everyone.body.contains(r#"/punishments/assign"#),
        "everyone tab must render the assign form: {}",
        everyone.body
    );
    let catalog = request(
        server.addr,
        "GET",
        "/punishments?tab=catalog",
        &[("Cookie", cookie.as_str())],
        b"",
    )
    .await;
    assert_eq!(catalog.status, 200);
    assert!(
        catalog.body.contains("20 pushups"),
        "catalog tab must show items: {}",
        catalog.body
    );
    assert!(
        !catalog.body.contains("bob chore"),
        "catalog tab must not show penalties: {}",
        catalog.body
    );
    let unknown = request(
        server.addr,
        "GET",
        "/punishments?tab=bogus",
        &[("Cookie", cookie.as_str())],
        b"",
    )
    .await;
    assert_eq!(unknown.status, 200);
    assert!(
        unknown.body.contains("alice chore"),
        "unknown tab must fall back to mine: {}",
        unknown.body
    );
    assert!(
        !unknown.body.contains("bob chore"),
        "unknown tab must fall back to mine: {}",
        unknown.body
    );
}

#[tokio::test]
async fn ssr_assign_creates_manual_penalty_and_redirects_to_everyone() {
    let server = setup("ssr-assign").await;
    let alice = signup_user(&server, "assigner").await;
    let bob = signup_user(&server, "assignee").await;
    let bob_id = user_id(&server, &bob).await;
    let ids = catalog_ids(&server).await;
    let cookie = format!("session={alice}");
    let response = request(
        server.addr,
        "POST",
        "/punishments/assign",
        &[
            ("Cookie", cookie.as_str()),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        format!("user_id={bob_id}&custom_title=mow%20the%20lawn&tab=everyone").as_bytes(),
    )
    .await;
    assert_eq!(response.status, 303, "assign must redirect: {response:?}");
    assert_eq!(
        response.header("location").as_deref(),
        Some("/punishments?tab=everyone"),
        "assign must land on the everyone tab"
    );
    let flash = response.header("set-cookie").unwrap_or_default();
    assert!(
        flash.contains("Penalty%20assigned"),
        "flash must say the penalty was assigned: {flash}"
    );
    let by_catalog = request(
        server.addr,
        "POST",
        "/punishments/assign",
        &[
            ("Cookie", cookie.as_str()),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        format!("user_id={bob_id}&catalog_id={}&tab=everyone", ids[0]).as_bytes(),
    )
    .await;
    assert_eq!(
        by_catalog.status, 303,
        "catalog assign failed: {by_catalog:?}"
    );
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT user_id, source, status FROM punishments WHERE custom_title = 'mow the lawn'",
    )
    .fetch_all(&server.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "custom assign must insert exactly one row");
    assert_eq!(
        rows[0].0, bob_id,
        "penalty must belong to the selected user"
    );
    assert_eq!(rows[0].1, "manual", "ssr assign must be manual");
    assert_eq!(rows[0].2, "pending", "ssr assign must be pending");
    let catalog_rows: Vec<(Option<i64>,)> = sqlx::query_as(
        "SELECT catalog_id FROM punishments WHERE user_id = ? AND catalog_id IS NOT NULL",
    )
    .bind(bob_id)
    .fetch_all(&server.pool)
    .await
    .unwrap();
    assert_eq!(catalog_rows.len(), 1, "catalog assign must insert one row");
    assert_eq!(catalog_rows[0].0, Some(ids[0]));
}

#[tokio::test]
async fn ssr_assign_validation_renders_errors_without_inserting() {
    let server = setup("ssr-assign-invalid").await;
    let alice = signup_user(&server, "validator").await;
    let bob = signup_user(&server, "targetuser").await;
    let bob_id = user_id(&server, &bob).await;
    let ids = catalog_ids(&server).await;
    let cookie = format!("session={alice}");
    let headers = [
        ("Cookie", cookie.as_str()),
        ("Content-Type", "application/x-www-form-urlencoded"),
    ];
    let both = request(
        server.addr,
        "POST",
        "/punishments/assign",
        &headers,
        format!(
            "user_id={bob_id}&catalog_id={}&custom_title=both&tab=everyone",
            ids[0]
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        both.status, 422,
        "both fields must render an error: {both:?}"
    );
    assert!(
        both.body
            .contains("Pick a catalog item or enter a custom title"),
        "error message must render: {}",
        both.body
    );
    let neither = request(
        server.addr,
        "POST",
        "/punishments/assign",
        &headers,
        format!("user_id={bob_id}&tab=everyone").as_bytes(),
    )
    .await;
    assert_eq!(
        neither.status, 422,
        "neither field must render an error: {neither:?}"
    );
    assert!(
        neither
            .body
            .contains("Pick a catalog item or enter a custom title"),
        "error message must render: {}",
        neither.body
    );
    let ghost = request(
        server.addr,
        "POST",
        "/punishments/assign",
        &headers,
        b"user_id=99999&custom_title=ghost&tab=everyone",
    )
    .await;
    assert_eq!(
        ghost.status, 422,
        "unknown user must render an error: {ghost:?}"
    );
    assert!(
        ghost.body.contains("Choose a member"),
        "member error must render: {}",
        ghost.body
    );
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM punishments")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "invalid submissions must not create punishments");
}

#[tokio::test]
async fn ssr_clear_and_waive_redirect_to_origin_tab_with_new_flash() {
    let server = setup("ssr-close").await;
    let token = signup_user(&server, "closer").await;
    let target = user_id(&server, &token).await;
    let assign = json_call(
        &server,
        "POST",
        &format!("/api/v1/users/{target}/punishments"),
        &token,
        serde_json::json!({ "custom_title": "dust the shelf" }),
    )
    .await;
    assert_eq!(assign.status, 200);
    let assign2 = json_call(
        &server,
        "POST",
        &format!("/api/v1/users/{target}/punishments"),
        &token,
        serde_json::json!({ "custom_title": "sweep the porch" }),
    )
    .await;
    assert_eq!(assign2.status, 200);
    let id = assign.json()["id"].as_i64().unwrap();
    let id2 = assign2.json()["id"].as_i64().unwrap();
    let cookie = format!("session={token}");
    let headers = [
        ("Cookie", cookie.as_str()),
        ("Content-Type", "application/x-www-form-urlencoded"),
    ];
    let clear = request(
        server.addr,
        "POST",
        &format!("/punishments/{id}/clear"),
        &headers,
        b"tab=mine",
    )
    .await;
    assert_eq!(clear.status, 303, "clear failed: {clear:?}");
    assert_eq!(
        clear.header("location").as_deref(),
        Some("/punishments?tab=mine")
    );
    let flash = clear.header("set-cookie").unwrap_or_default();
    assert!(
        flash.contains("Penalty%20cleared"),
        "flash must say the penalty was cleared: {flash}"
    );
    let waive = request(
        server.addr,
        "POST",
        &format!("/punishments/{id2}/waive"),
        &headers,
        b"tab=everyone",
    )
    .await;
    assert_eq!(waive.status, 303, "waive failed: {waive:?}");
    assert_eq!(
        waive.header("location").as_deref(),
        Some("/punishments?tab=everyone")
    );
    let flash = waive.header("set-cookie").unwrap_or_default();
    assert!(
        flash.contains("Penalty%20waived"),
        "flash must say the penalty was waived: {flash}"
    );
    let (status,): (String,) = sqlx::query_as("SELECT status FROM punishments WHERE id = ?")
        .bind(id)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(status, "cleared");
    let (status,): (String,) = sqlx::query_as("SELECT status FROM punishments WHERE id = ?")
        .bind(id2)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(status, "waived");
}

async fn seed_tick(pool: &SqlitePool, routine_id: i64, date: NaiveDate) -> i64 {
    let (user_id,): (i64,) = sqlx::query_as("SELECT user_id FROM routines WHERE id = ?")
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

#[tokio::test]
async fn cap_counts_per_missed_day_not_per_evaluation() {
    let server = setup("cap-per-day").await;
    let token = signup_user(&server, "pia").await;
    let id = all_weekdays_routine(&server, &token).await;
    for offset in 1..=4u64 {
        insert_past_occurrence(&server, id, today_utc() - Days::new(offset)).await;
    }
    trigger_eval(&server, &token).await;
    let drawn = count_scalar(
        &server,
        "SELECT COUNT(*) FROM punishments WHERE source = 'auto' AND catalog_id IS NOT NULL",
    )
    .await;
    assert_eq!(drawn, 4, "one miss on each of four days must each draw");
    let days = count_scalar(
        &server,
        "SELECT COUNT(DISTINCT miss_day) FROM punishments WHERE source = 'auto'",
    )
    .await;
    assert_eq!(days, 4, "miss_day must be the occurrence date");
}

#[tokio::test]
async fn count_miss_over_cap_is_visible_and_linked_to_routine() {
    let server = setup("count-overflow").await;
    let token = signup_user(&server, "quin").await;
    let prev_monday = week_monday(today_utc()) - Days::new(7);
    let prev_sunday = prev_monday + Days::new(6);
    for _ in 0..3 {
        let routine = all_weekdays_routine(&server, &token).await;
        insert_past_occurrence(&server, routine, prev_sunday).await;
    }
    let goal = create_routine(
        &server,
        &token,
        serde_json::json!({ "title": "swim", "kind": "count", "per_period": 2, "period": "week" }),
    )
    .await;
    backdate_routine_start(&server, goal, prev_monday).await;
    trigger_eval(&server, &token).await;
    let row: (Option<i64>, Option<i64>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT catalog_id, routine_id, period_start, miss_day FROM punishments WHERE source = 'auto' AND occurrence_id IS NULL",
    )
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(row.0, None, "fourth miss on the same day must not draw");
    assert_eq!(row.1, Some(goal));
    assert_eq!(row.2, Some(prev_monday.to_string()));
    assert_eq!(row.3, Some(prev_sunday.to_string()));
    let cookie = format!("session={token}");
    let page = request(server.addr, "GET", "/punishments", &[("Cookie", cookie.as_str())], b"").await;
    assert_eq!(page.status, 200);
    assert!(
        page.body.contains(&format!("missed swim goal in the week of {prev_monday}")),
        "goal miss must be labelled: {}",
        page.body
    );
    assert!(page.body.contains("Missed, no penalty drawn"));
}

#[tokio::test]
async fn draws_prefer_penalties_not_already_pending() {
    let server = setup("draw-spread").await;
    let token = signup_user(&server, "rory").await;
    let catalog = catalog_ids(&server).await.len();
    let id = all_weekdays_routine(&server, &token).await;
    for offset in 1..=u64::try_from(catalog).unwrap() {
        insert_past_occurrence(&server, id, today_utc() - Days::new(offset)).await;
    }
    trigger_eval(&server, &token).await;
    let distinct = count_scalar(
        &server,
        "SELECT COUNT(DISTINCT catalog_id) FROM punishments WHERE source = 'auto' AND catalog_id IS NOT NULL",
    )
    .await;
    assert_eq!(
        distinct,
        i64::try_from(catalog).unwrap(),
        "every catalog item must be used once before any repeats"
    );
}

#[tokio::test]
async fn history_shows_what_was_missed() {
    let server = setup("history-source").await;
    let token = signup_user(&server, "sam").await;
    let id = all_weekdays_routine(&server, &token).await;
    let missed = today_utc() - Days::new(1);
    insert_past_occurrence(&server, id, missed).await;
    trigger_eval(&server, &token).await;
    let (penalty,): (i64,) = sqlx::query_as("SELECT id FROM punishments WHERE source = 'auto'")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    let cleared = json_call(
        &server,
        "POST",
        &format!("/api/v1/punishments/{penalty}/clear"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(cleared.status, 200, "clear failed: {cleared:?}");
    let cookie = format!("session={token}");
    let page = request(server.addr, "GET", "/punishments", &[("Cookie", cookie.as_str())], b"").await;
    let history = page.body.split("<h2>History</h2>").nth(1).unwrap_or_default();
    assert!(
        history.contains(&format!("missed daily drill on {missed}")),
        "history must name the miss: {history}"
    );
}
