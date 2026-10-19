mod common;

use std::net::SocketAddr;
use std::time::Duration;

use chrono::Datelike;
use chrono::Days;
use chrono::NaiveDate;
use chrono::Utc;
use chrono::Weekday;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use sqlx::PgPool;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use routinely::app::AppState;
use routinely::db;

const SIGNUP_CODE: &str = "test-code";
const PASSWORD: &str = "password123";
const PNG_BYTES: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

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

async fn signup_user(server: &Server, username: &str, timezone: &str) -> String {
    let body = serde_json::json!({
        "username": username,
        "password": PASSWORD,
        "code": SIGNUP_CODE,
        "timezone": timezone,
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

async fn create_routine(server: &Server, token: &str, payload: Value) -> i64 {
    let response = json_call(server, "POST", "/api/v1/routines", token, payload).await;
    assert_eq!(response.status, 200, "routine create failed: {response:?}");
    return response.json()["id"].as_i64().unwrap();
}

async fn day_json(server: &Server, token: &str, date: Option<NaiveDate>) -> Value {
    let path = match date {
        Some(d) => format!("/api/v1/day?date={d}"),
        None => "/api/v1/day".to_string(),
    };
    let response = json_call(server, "GET", &path, token, serde_json::json!({})).await;
    assert_eq!(response.status, 200, "day json failed: {response:?}");
    return response.json();
}

fn routine_entry(day: &Value, routine_id: i64) -> Option<Value> {
    for routine in day["routines"].as_array().unwrap() {
        if routine["id"].as_i64() == Some(routine_id) {
            return Some(routine.clone());
        }
    }
    return None;
}

fn today_utc() -> NaiveDate {
    return Utc::now().date_naive();
}

fn week_monday(today: NaiveDate) -> NaiveDate {
    let offset = today.weekday().num_days_from_monday();
    return today - Days::new(u64::from(offset));
}

fn all_weekdays() -> Value {
    return serde_json::json!([0, 1, 2, 3, 4, 5, 6]);
}

fn multipart_body(boundary: &str, parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, filename, content) in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match filename {
            Some(fname) => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{fname}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
                    )
                    .as_bytes(),
                );
            }
            None => {
                body.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
                );
            }
        }
        body.extend_from_slice(content);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    return body;
}

async fn count_all(server: &Server, sql: &'static str) -> i64 {
    let (value,): (i64,) = sqlx::query_as(sql).fetch_one(&server.pool).await.unwrap();
    return value;
}

async fn backdate_start(server: &Server, routine_id: i64, date: NaiveDate) {
    sqlx::query("UPDATE routines SET start_date = $1 WHERE id = $2")
        .bind(date.to_string())
        .bind(routine_id)
        .execute(&server.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn weekdays_routine_appears_on_matching_day_and_occurrence_persists() {
    let server = setup("weekdays-day").await;
    let token = signup_user(&server, "alice", "UTC").await;
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "gym",
            "kind": "weekdays",
            "weekdays": [0, 2, 4],
        }),
    )
    .await;
    let today = today_utc();
    let mut date = today;
    while ![Weekday::Mon, Weekday::Wed, Weekday::Fri].contains(&date.weekday()) {
        date = date.pred_opt().unwrap();
    }
    backdate_start(&server, id, date - Days::new(7)).await;
    let day = day_json(&server, &token, Some(date)).await;
    let entry = routine_entry(&day, id);
    assert!(entry.is_some(), "routine missing from day: {day}");
    let entry = entry.unwrap();
    assert_eq!(entry["kind"], "weekdays");
    assert_eq!(entry["completed"], false);
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM occurrences WHERE routine_id = $1 AND due_date = $2")
            .bind(id)
            .bind(date.to_string())
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(count, 1, "occurrence not persisted for {date}");
}

#[tokio::test]
async fn count_routine_shows_progress_and_two_ticks_make_two_occurrences() {
    let server = setup("count-progress").await;
    let token = signup_user(&server, "bobby", "UTC").await;
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
    let today = today_utc();
    let monday = week_monday(today);
    let page = get_page(&server, &format!("/day?date={today}"), &token).await;
    assert_eq!(page.status, 200);
    assert!(page.body.contains("0/2"), "progress missing: {}", page.body);
    let day = day_json(&server, &token, Some(today)).await;
    let entry = routine_entry(&day, id).unwrap();
    assert_eq!(entry["period_done"], 0);
    assert_eq!(entry["period_required"], 2);
    seed_tick(&server.pool, id, monday).await;
    let second_day = monday + Days::new(1);
    seed_tick(&server.pool, id, second_day).await;
    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM occurrences WHERE routine_id = $1")
        .bind(id)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(rows, 2, "expected two occurrences in the period");
    let (same_period,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM occurrences WHERE routine_id = $1 AND period_start = $2",
    )
    .bind(id)
    .bind(monday.to_string())
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(same_period, 2, "both occurrences share the period start");
    let day = day_json(&server, &token, Some(today)).await;
    let entry = routine_entry(&day, id).unwrap();
    assert_eq!(entry["period_done"], 2);
    assert_eq!(entry["period_required"], 2);
}

#[tokio::test]
async fn tick_retick_untick_and_pin_cascade() {
    let server = setup("tick-cycle").await;
    let token = signup_user(&server, "carol", "UTC").await;
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "meditate",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
        }),
    )
    .await;
    let day = day_json(&server, &token, None).await;
    let occurrence_id = routine_entry(&day, id).unwrap()["occurrence_id"]
        .as_i64()
        .expect("occurrence id in day json");
    let first = json_call(
        &server,
        "PUT",
        &format!("/api/v1/occurrences/{id}/completion"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(first.status, 200);
    assert_eq!(
        count_all(&server, "SELECT COUNT(*) FROM completions").await,
        1
    );
    let retick = json_call(
        &server,
        "PUT",
        &format!("/api/v1/occurrences/{id}/completion"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(retick.status, 200, "re-tick must be a no-op success");
    assert_eq!(
        count_all(&server, "SELECT COUNT(*) FROM completions").await,
        1,
        "re-tick must not create a second completion"
    );
    let pin = json_call(
        &server,
        "POST",
        &format!("/api/v1/occurrences/{occurrence_id}/pin"),
        &token,
        serde_json::json!({ "summary": "done outside" }),
    )
    .await;
    assert_eq!(pin.status, 200, "pin failed: {pin:?}");
    let untick = json_call(
        &server,
        "DELETE",
        &format!("/api/v1/occurrences/{id}/completion"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(untick.status, 200);
    assert_eq!(
        count_all(&server, "SELECT COUNT(*) FROM completions").await,
        0
    );
    assert_eq!(count_all(&server, "SELECT COUNT(*) FROM pins").await, 0);
    assert_eq!(
        count_all(&server, "SELECT COUNT(*) FROM attachments").await,
        0
    );
    assert_eq!(
        count_all(&server, "SELECT COUNT(*) FROM occurrences").await,
        1,
        "occurrence row must survive untick"
    );
}

#[tokio::test]
async fn evidence_note_and_pin_rules() {
    let server = setup("evidence-pin").await;
    let alice = signup_user(&server, "dave", "UTC").await;
    let id = create_routine(
        &server,
        &alice,
        serde_json::json!({
            "title": "run",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
        }),
    )
    .await;
    backdate_start(&server, id, today_utc() - Days::new(2)).await;
    let ticked = json_call(
        &server,
        "PUT",
        &format!("/api/v1/occurrences/{id}/completion"),
        &alice,
        serde_json::json!({ "evidence_note": "ran 5k" }),
    )
    .await;
    assert_eq!(ticked.status, 200);
    let (note,): (String,) =
        sqlx::query_as("SELECT evidence_note FROM completions WHERE evidence_note IS NOT NULL")
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(note, "ran 5k");
    let day = day_json(&server, &alice, None).await;
    let occurrence_id = routine_entry(&day, id).unwrap()["occurrence_id"]
        .as_i64()
        .unwrap();
    let pin = json_call(
        &server,
        "POST",
        &format!("/api/v1/occurrences/{occurrence_id}/pin"),
        &alice,
        serde_json::json!({ "summary": "morning run", "link_url": "https://example.com/route" }),
    )
    .await;
    assert_eq!(pin.status, 200, "pin failed: {pin:?}");
    let (summary,): (String,) = sqlx::query_as("SELECT summary FROM pins")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(summary, "morning run");
    let (link_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM attachments WHERE kind = 'link'")
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(link_count, 1);
    let yesterday = today_utc() - Days::new(1);
    let past = day_json(&server, &alice, Some(yesterday)).await;
    let past_occurrence = routine_entry(&past, id).unwrap()["occurrence_id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        past["routines"][0]["completed"], false,
        "yesterday must be unticked for the 409 check"
    );
    let uncompleted = json_call(
        &server,
        "POST",
        &format!("/api/v1/occurrences/{past_occurrence}/pin"),
        &alice,
        serde_json::json!({ "summary": "too early" }),
    )
    .await;
    assert_eq!(
        uncompleted.status, 409,
        "pin on uncompleted occurrence must fail"
    );
    let bob = signup_user(&server, "erin", "UTC").await;
    let bob_routine = create_routine(
        &server,
        &bob,
        serde_json::json!({
            "title": "bob run",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
        }),
    )
    .await;
    let bob_day = day_json(&server, &bob, None).await;
    let bob_occurrence = routine_entry(&bob_day, bob_routine).unwrap()["occurrence_id"]
        .as_i64()
        .unwrap();
    let foreign = json_call(
        &server,
        "POST",
        &format!("/api/v1/occurrences/{bob_occurrence}/pin"),
        &alice,
        serde_json::json!({ "summary": "sneaky" }),
    )
    .await;
    assert_eq!(foreign.status, 404, "pin on foreign occurrence must 404");
}

#[tokio::test]
async fn photo_upload_stores_file_and_rejects_txt() {
    let server = setup("photo-upload").await;
    let token = signup_user(&server, "frank", "UTC").await;
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "cook",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
        }),
    )
    .await;
    let day = day_json(&server, &token, None).await;
    let occurrence_id = routine_entry(&day, id).unwrap()["occurrence_id"]
        .as_i64()
        .unwrap();
    let tick = json_call(
        &server,
        "PUT",
        &format!("/api/v1/occurrences/{id}/completion"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(tick.status, 200, "tick before pin failed: {tick:?}");
    let boundary = "routinelyboundary42";
    let body = multipart_body(
        boundary,
        &[
            ("summary", None, b"meal done".as_slice()),
            ("photos", Some("shot.png"), PNG_BYTES),
        ],
    );
    let cookie = format!("session={token}");
    let response = request(
        server.addr,
        "POST",
        &format!("/occurrences/{occurrence_id}/pin"),
        &[
            ("Cookie", cookie.as_str()),
            (
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            ),
        ],
        &body,
    )
    .await;
    assert_eq!(response.status, 303, "photo pin failed: {response:?}");
    let digest = Sha256::digest(PNG_BYTES);
    let mut expected_name = String::new();
    for byte in digest {
        expected_name.push_str(&format!("{byte:02x}"));
    }
    expected_name.push_str(".png");
    let stored = std::path::Path::new("data/uploads").join(&expected_name);
    assert!(stored.exists(), "uploaded file missing at {stored:?}");
    let (file_path,): (String,) = sqlx::query_as(
        "SELECT file_path FROM attachments WHERE kind = 'photo' AND file_path IS NOT NULL",
    )
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(file_path, expected_name);
    let bad = multipart_body(
        boundary,
        &[
            ("summary", None, b"notes file".as_slice()),
            ("photos", Some("notes.txt"), b"plain text".as_slice()),
        ],
    );
    let response = request(
        server.addr,
        "POST",
        &format!("/occurrences/{occurrence_id}/pin"),
        &[
            ("Cookie", cookie.as_str()),
            (
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            ),
        ],
        &bad,
    )
    .await;
    assert_eq!(
        response.status, 422,
        "txt upload must be rejected: {response:?}"
    );
}

#[tokio::test]
async fn day_view_isolates_users() {
    let server = setup("isolation").await;
    let alice = signup_user(&server, "grace", "UTC").await;
    create_routine(
        &server,
        &alice,
        serde_json::json!({
            "title": "alice-secret",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
        }),
    )
    .await;
    let bob = signup_user(&server, "heidi", "UTC").await;
    let bob_day = day_json(&server, &bob, None).await;
    assert!(
        routine_entry(&bob_day, 1).is_none(),
        "bob sees alice's routine: {bob_day}"
    );
    assert!(bob_day["routines"].as_array().unwrap().is_empty());
    let page = get_page(&server, "/day", &bob).await;
    assert_eq!(page.status, 200);
    assert!(
        !page.body.contains("alice-secret"),
        "SSR leaks alice routine: {}",
        page.body
    );
    let alice_day = day_json(&server, &alice, None).await;
    assert!(routine_entry(&alice_day, 1).is_some());
}

#[tokio::test]
async fn todos_crud_ordering_and_timezone_deadline() {
    let server = setup("todos").await;
    let token = signup_user(&server, "ivan", "Europe/Berlin").await;
    let created = json_call(
        &server,
        "POST",
        "/api/v1/todos",
        &token,
        serde_json::json!({ "title": "early", "deadline": "2026-01-02T03:00" }),
    )
    .await;
    assert_eq!(created.status, 200, "todo create failed: {created:?}");
    let (deadline,): (String,) = sqlx::query_as("SELECT deadline FROM todos WHERE title = 'early'")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        deadline, "2026-01-02T02:00:00+00:00",
        "Berlin 03:00 must store 02:00 UTC"
    );
    let early_id = created.json()["id"].as_i64().unwrap();
    let overdue_id = json_call(
        &server,
        "POST",
        "/api/v1/todos",
        &token,
        serde_json::json!({ "title": "old", "deadline": "2020-01-01T10:00" }),
    )
    .await
    .json()["id"]
        .as_i64()
        .unwrap();
    let future_id = json_call(
        &server,
        "POST",
        "/api/v1/todos",
        &token,
        serde_json::json!({ "title": "future", "deadline": "2030-05-05T10:00" }),
    )
    .await
    .json()["id"]
        .as_i64()
        .unwrap();
    let plain_id = json_call(
        &server,
        "POST",
        "/api/v1/todos",
        &token,
        serde_json::json!({ "title": "plain" }),
    )
    .await
    .json()["id"]
        .as_i64()
        .unwrap();
    let done_id = json_call(
        &server,
        "POST",
        "/api/v1/todos",
        &token,
        serde_json::json!({ "title": "washed" }),
    )
    .await
    .json()["id"]
        .as_i64()
        .unwrap();
    let toggled = json_call(
        &server,
        "POST",
        &format!("/api/v1/todos/{done_id}/toggle"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(toggled.status, 200);
    let day = day_json(&server, &token, None).await;
    let order: Vec<i64> = day["todos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        order,
        vec![overdue_id, early_id, future_id, plain_id, done_id],
        "overdue first (deadline asc), then pending by deadline, done last: {day}"
    );
    assert_eq!(day["todos"][0]["overdue"], true);
    let toggle = json_call(
        &server,
        "POST",
        &format!("/api/v1/todos/{plain_id}/toggle"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(toggle.status, 200);
    let (completed_at,): (Option<String>,) =
        sqlx::query_as("SELECT completed_at FROM todos WHERE id = $1")
            .bind(plain_id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert!(completed_at.is_some(), "toggle must set completed_at");
    let untoggle = json_call(
        &server,
        "POST",
        &format!("/api/v1/todos/{plain_id}/toggle"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(untoggle.status, 200);
    let (cleared,): (Option<String>,) =
        sqlx::query_as("SELECT completed_at FROM todos WHERE id = $1")
            .bind(plain_id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert!(cleared.is_none(), "second toggle must clear completed_at");
    let deleted = json_call(
        &server,
        "DELETE",
        &format!("/api/v1/todos/{overdue_id}"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(deleted.status, 200);
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM todos WHERE id = $1")
        .bind(overdue_id)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn note_upserts_single_row_per_day() {
    let server = setup("notes").await;
    let token = signup_user(&server, "judy", "UTC").await;
    let first = json_call(
        &server,
        "PUT",
        "/api/v1/notes/2026-09-22",
        &token,
        serde_json::json!({ "body": "one" }),
    )
    .await;
    assert_eq!(first.status, 200);
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM daily_notes")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let second = json_call(
        &server,
        "PUT",
        "/api/v1/notes/2026-09-22",
        &token,
        serde_json::json!({ "body": "two" }),
    )
    .await;
    assert_eq!(second.status, 200);
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM daily_notes")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "upsert must not duplicate rows");
    let (body,): (String,) = sqlx::query_as("SELECT body FROM daily_notes")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(body, "two");
    let cookie = format!("session={token}");
    let response = request(
        server.addr,
        "POST",
        "/notes/2026-09-22",
        &[
            ("Cookie", cookie.as_str()),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        b"body=three",
    )
    .await;
    assert_eq!(response.status, 303, "SSR note save failed: {response:?}");
    let (body,): (String,) = sqlx::query_as("SELECT body FROM daily_notes")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(body, "three");
}

#[tokio::test]
async fn archive_hides_routine_and_stops_materialization() {
    let server = setup("archive").await;
    let token = signup_user(&server, "ken", "UTC").await;
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "temporary",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
        }),
    )
    .await;
    let archived = json_call(
        &server,
        "POST",
        &format!("/api/v1/routines/{id}/archive"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(archived.status, 200);
    let day = day_json(&server, &token, None).await;
    assert!(
        routine_entry(&day, id).is_none(),
        "archived routine visible: {day}"
    );
    let page = get_page(&server, "/day", &token).await;
    assert!(
        !page.body.contains("temporary"),
        "SSR shows archived routine"
    );
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM occurrences WHERE routine_id = $1")
        .bind(id)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "archived routine must not materialize");
    let (archived_at,): (Option<String>,) =
        sqlx::query_as("SELECT archived_at FROM routines WHERE id = $1")
            .bind(id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert!(archived_at.is_some(), "archived_at must be set");
}

#[tokio::test]
async fn date_navigation_shows_missed_and_planned() {
    let server = setup("navigation").await;
    let token = signup_user(&server, "lena", "UTC").await;
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "navcheck",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
        }),
    )
    .await;
    let tomorrow = today_utc() + Days::new(1);
    backdate_start(&server, id, today_utc() - Days::new(7)).await;
    let yesterday = today_utc() - Days::new(1);
    let past = get_page(&server, &format!("/day?date={yesterday}"), &token).await;
    assert_eq!(past.status, 200);
    assert!(
        past.body.contains("status-missed"),
        "past unticked must be missed: {}",
        past.body
    );
    let future = get_page(&server, &format!("/day?date={tomorrow}"), &token).await;
    assert_eq!(future.status, 200);
    assert!(
        future.body.contains("status-open"),
        "future planned must be open: {}",
        future.body
    );
    let today = today_utc();
    let today_page = get_page(&server, &format!("/day?date={today}"), &token).await;
    assert!(today_page.body.contains("status-open"));
}

#[tokio::test]
async fn unauth_requests_redirect_or_401() {
    let server = setup("unauth").await;
    let page = request(server.addr, "GET", "/day", &[], b"").await;
    assert_eq!(page.status, 303);
    assert_eq!(page.header("location").as_deref(), Some("/login"));
    let api = request(server.addr, "GET", "/api/v1/day", &[], b"").await;
    assert_eq!(api.status, 401);
    let root = request(server.addr, "GET", "/", &[], b"").await;
    assert_eq!(root.status, 303);
    assert_eq!(root.header("location").as_deref(), Some("/login"));
}

#[tokio::test]
async fn routine_create_validation_rules() {
    let server = setup("validation").await;
    let token = signup_user(&server, "mallory", "UTC").await;
    let zero_weekdays = json_call(
        &server,
        "POST",
        "/api/v1/routines",
        &token,
        serde_json::json!({ "title": "x", "kind": "weekdays", "weekdays": [] }),
    )
    .await;
    assert_eq!(zero_weekdays.status, 422, "zero weekdays must 422");
    let zero_iterations = json_call(
        &server,
        "POST",
        "/api/v1/routines",
        &token,
        serde_json::json!({
            "title": "x",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
            "iterations": 0,
        }),
    )
    .await;
    assert_eq!(zero_iterations.status, 422, "iterations 0 must 422");
    let past_end = json_call(
        &server,
        "POST",
        "/api/v1/routines",
        &token,
        serde_json::json!({
            "title": "x",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
            "end_date": "2000-01-01",
        }),
    )
    .await;
    assert_eq!(past_end.status, 422, "past end_date must 422");
    let time_end_only = json_call(
        &server,
        "POST",
        "/api/v1/routines",
        &token,
        serde_json::json!({
            "title": "x",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
            "time_end": "18:00",
        }),
    )
    .await;
    assert_eq!(
        time_end_only.status, 422,
        "time_end without time_of_day must 422"
    );
    let valid = json_call(
        &server,
        "POST",
        "/api/v1/routines",
        &token,
        serde_json::json!({
            "title": "ok weekdays",
            "kind": "weekdays",
            "weekdays": [0, 2, 4],
            "time_of_day": "07:30",
        }),
    )
    .await;
    assert_eq!(valid.status, 200, "valid weekdays create failed: {valid:?}");
    let valid_count = json_call(
        &server,
        "POST",
        "/api/v1/routines",
        &token,
        serde_json::json!({
            "title": "ok count",
            "kind": "count",
            "per_period": 3,
            "period": "day",
        }),
    )
    .await;
    assert_eq!(
        valid_count.status, 200,
        "valid count create failed: {valid_count:?}"
    );
}

#[test]
fn overlap_columns_two_overlapping_and_one_separate() {
    let spans = [(0, 60), (30, 90), (120, 150)];
    let result = routinely::web::day::overlap_columns(&spans);
    assert_eq!(
        result,
        vec![(1, 2), (2, 2), (1, 1)],
        "overlap must spread into columns per cluster"
    );
}

#[tokio::test]
async fn onboarding_checklist_shown_for_fresh_user() {
    let server = setup("onboarding-fresh").await;
    let token = signup_user(&server, "onboardfresh", "UTC").await;
    let page = get_page(&server, "/day", &token).await;
    assert_eq!(page.status, 200, "day page failed: {page:?}");
    assert!(
        page.body.contains(r#"class="card onboarding""#),
        "fresh user must see the checklist: {}",
        page.body
    );
    assert!(
        page.body.contains("Create your first routine"),
        "checklist steps missing: {}",
        page.body
    );
    let (dismissed,): (Option<String>,) =
        sqlx::query_as("SELECT onboarding_dismissed_at FROM users WHERE username = 'onboardfresh'")
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert!(dismissed.is_none(), "column must start NULL");
}

#[tokio::test]
async fn onboarding_dismiss_sets_column_and_hides_checklist() {
    let server = setup("onboarding-dismiss").await;
    let token = signup_user(&server, "onboarder", "UTC").await;
    let cookie = format!("session={token}");
    let response = request(
        server.addr,
        "POST",
        "/onboarding/dismiss",
        &[
            ("Cookie", cookie.as_str()),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        b"",
    )
    .await;
    assert_eq!(response.status, 303, "dismiss must redirect: {response:?}");
    assert_eq!(response.header("location").as_deref(), Some("/day"));
    let (dismissed,): (Option<String>,) =
        sqlx::query_as("SELECT onboarding_dismissed_at FROM users WHERE username = 'onboarder'")
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert!(dismissed.is_some(), "dismiss must set the column");
    let page = get_page(&server, "/day", &token).await;
    assert_eq!(page.status, 200);
    assert!(
        !page.body.contains(r#"class="card onboarding""#),
        "checklist must be hidden after dismiss: {}",
        page.body
    );
    let api = json_call(
        &server,
        "POST",
        "/api/v1/me/onboarding/dismiss",
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(api.status, 200, "api dismiss failed: {api:?}");
    assert_eq!(api.json()["ok"], true);
}

#[tokio::test]
async fn htmx_tick_returns_routines_fragment_and_updates_state() {
    let server = setup("htmx-tick").await;
    let token = signup_user(&server, "htmxer", "UTC").await;
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "htmx gym",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
        }),
    )
    .await;
    let cookie = format!("session={token}");
    let tick = request(
        server.addr,
        "POST",
        &format!("/occurrences/{id}/tick"),
        &[
            ("Cookie", cookie.as_str()),
            ("HX-Request", "true"),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        b"",
    )
    .await;
    assert_eq!(tick.status, 200, "htmx tick failed: {tick:?}");
    assert!(
        tick.body.contains(r#"id="routines""#),
        "tick must return the routines region: {}",
        tick.body
    );
    assert!(
        tick.body.contains("htmx gym"),
        "fragment must contain the routine: {}",
        tick.body
    );
    let (completions,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM completions c JOIN occurrences o ON o.id = c.occurrence_id \
         WHERE o.routine_id = $1",
    )
    .bind(id)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(completions, 1, "tick must create a completion");
    let untick = request(
        server.addr,
        "DELETE",
        &format!("/occurrences/{id}/tick"),
        &[("Cookie", cookie.as_str()), ("HX-Request", "true")],
        b"",
    )
    .await;
    assert_eq!(untick.status, 200, "htmx untick failed: {untick:?}");
    let (completions,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM completions c JOIN occurrences o ON o.id = c.occurrence_id \
         WHERE o.routine_id = $1",
    )
    .bind(id)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(completions, 0, "untick must remove the completion");
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

#[tokio::test]
async fn ticking_is_limited_to_today() {
    let server = setup("tick-today-only").await;
    let token = signup_user(&server, "tess", "UTC").await;
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({ "title": "stretch", "kind": "weekdays", "weekdays": all_weekdays() }),
    )
    .await;
    let today = today_utc();
    backdate_start(&server, id, today - Days::new(3)).await;
    let yesterday = today - Days::new(1);
    let tomorrow = today + Days::new(1);
    for day in [yesterday, tomorrow] {
        let put = json_call(
            &server,
            "PUT",
            &format!("/api/v1/occurrences/{id}/completion?date={day}"),
            &token,
            serde_json::json!({}),
        )
        .await;
        assert_eq!(put.status, 422, "tick on {day} must be rejected: {put:?}");
        let delete = json_call(
            &server,
            "DELETE",
            &format!("/api/v1/occurrences/{id}/completion?date={day}"),
            &token,
            serde_json::json!({}),
        )
        .await;
        assert_eq!(delete.status, 422, "untick on {day} must be rejected");
    }
    let completions = count_all(&server, "SELECT COUNT(*) FROM completions").await;
    assert_eq!(completions, 0, "rejected ticks must not write");
    let ok = json_call(
        &server,
        "PUT",
        &format!("/api/v1/occurrences/{id}/completion?date={today}"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(ok.status, 200, "tick today failed: {ok:?}");
    let cookie = format!("session={token}");
    let ssr = request(
        server.addr,
        "POST",
        &format!("/occurrences/{id}/tick?date={yesterday}"),
        &[
            ("Cookie", cookie.as_str()),
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Accept", "text/html"),
        ],
        b"",
    )
    .await;
    assert_eq!(ssr.status, 422);
    assert!(
        ssr.header("content-type").unwrap_or_default().starts_with("text/html"),
        "browser gets an html error page: {ssr:?}"
    );
    assert!(ssr.body.contains("routines can be changed"), "{}", ssr.body);
    let past = get_page(&server, &format!("/day?date={yesterday}"), &token).await;
    assert_eq!(past.status, 200);
    assert!(past.body.contains("locked-note"), "past day must show the lock note");
    assert!(
        !past.body.contains(&format!("/occurrences/{id}/tick?date={yesterday}")),
        "past day must not offer a tick form"
    );
    let current = get_page(&server, &format!("/day?date={today}"), &token).await;
    assert!(!current.body.contains("locked-note"));
    assert!(current.body.contains(&format!("/occurrences/{id}/untick?date={today}")));
}

#[tokio::test]
async fn html_error_page_for_browsers_json_for_api() {
    let server = setup("error-pages").await;
    let token = signup_user(&server, "erra", "UTC").await;
    let cookie = format!("session={token}");
    let browser = request(
        server.addr,
        "GET",
        "/routines/999999/edit",
        &[
            ("Cookie", cookie.as_str()),
            ("Accept", "text/html,application/xhtml+xml"),
            ("Referer", "http://test/routines"),
        ],
        b"",
    )
    .await;
    assert_eq!(browser.status, 404);
    assert!(browser.body.contains("Not found"), "{}", browser.body);
    assert!(browser.body.contains(r#"href="/routines""#), "back link from referer");
    let foreign = request(
        server.addr,
        "GET",
        "/routines/999999/edit",
        &[
            ("Cookie", cookie.as_str()),
            ("Accept", "text/html"),
            ("Referer", "http://evil.example/x"),
        ],
        b"",
    )
    .await;
    assert!(!foreign.body.contains("evil.example"), "cross-origin referer ignored");
    let api = get_page(&server, "/routines/999999/edit", &token).await;
    assert_eq!(api.status, 404);
    assert!(api.body.contains(r#""error":"not_found""#), "{}", api.body);
}
