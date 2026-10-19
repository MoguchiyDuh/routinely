mod common;

use std::net::SocketAddr;
use std::time::Duration;

use chrono::Datelike;
use chrono::Days;
use chrono::NaiveDate;
use chrono::Utc;
use chrono::Weekday;
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use routinely::app::AppState;
use routinely::db;
use routinely::recurrence::PeriodKind;
use routinely::recurrence::Schedule;
use routinely::recurrence::ScheduleKind;
use routinely::recurrence::mask_from_weekdays;
use routinely::score::describe_schedule;

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

async fn setup(_name: &str) -> Server {
    let config = common::config(SIGNUP_CODE, Duration::from_secs(600)).await;
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

async fn backdate_start(server: &Server, routine_id: i64, date: NaiveDate) {
    sqlx::query("UPDATE routines SET start_date = $1 WHERE id = $2")
        .bind(date.to_string())
        .bind(routine_id)
        .execute(&server.pool)
        .await
        .unwrap();
}

async fn tick(server: &Server, token: &str, routine_id: i64, date: NaiveDate) -> i64 {
    if date != routinely::store::user_today("UTC") {
        return seed_tick(&server.pool, routine_id, date).await;
    }
    let response = json_call(
        server,
        "PUT",
        &format!("/api/v1/occurrences/{routine_id}/completion?date={date}"),
        token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status, 200, "tick failed: {response:?}");
    return response.json()["occurrence_id"].as_i64().unwrap();
}

async fn pin_occurrence(server: &Server, token: &str, occurrence_id: i64, summary: &str) {
    let response = json_call(
        server,
        "POST",
        &format!("/api/v1/occurrences/{occurrence_id}/pin"),
        token,
        serde_json::json!({ "summary": summary, "link_url": "https://example.com/evidence" }),
    )
    .await;
    assert_eq!(response.status, 200, "pin failed: {response:?}");
}

async fn leaderboard_rows(server: &Server, token: &str) -> Vec<Value> {
    let response = json_call(
        server,
        "GET",
        "/api/v1/leaderboard",
        token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status, 200, "leaderboard failed: {response:?}");
    return response.json().as_array().expect("array").clone();
}

async fn profile_json(server: &Server, token: &str, user_id: i64) -> Value {
    let response = json_call(
        server,
        "GET",
        &format!("/api/v1/users/{user_id}"),
        token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status, 200, "profile failed: {response:?}");
    return response.json();
}

async fn user_id_by_name(server: &Server, username: &str) -> i64 {
    let (id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE username = $1")
        .bind(username)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    return id;
}

async fn add_punishments(server: &Server, user_id: i64, status: &str, count: usize) {
    for _ in 0..count {
        sqlx::query(
            "INSERT INTO punishments (user_id, catalog_id, custom_title, source, status, occurrence_id, assigned_at, cleared_at) \
             VALUES ($1, NULL, 'test punishment', 'manual', $2, NULL, $3, NULL)",
        )
        .bind(user_id)
        .bind(status)
        .bind(Utc::now().to_rfc3339())
        .execute(&server.pool)
        .await
        .unwrap();
    }
}

fn today_utc() -> NaiveDate {
    return Utc::now().date_naive();
}

fn past(offset: u64) -> NaiveDate {
    return today_utc() - Days::new(offset);
}

fn week_bounds(date: NaiveDate) -> (NaiveDate, NaiveDate) {
    let offset = u64::from(date.weekday().num_days_from_monday());
    let start = date - Days::new(offset);
    return (start, start + Days::new(6));
}

fn weekdays_all_payload(title: &str) -> Value {
    return serde_json::json!({
        "title": title,
        "kind": "weekdays",
        "weekdays": [0, 1, 2, 3, 4, 5, 6],
    });
}

fn count_payload(title: &str, per_period: u32, period: &str) -> Value {
    return serde_json::json!({
        "title": title,
        "kind": "count",
        "per_period": per_period,
        "period": period,
    });
}

fn row_for<'a>(rows: &'a [Value], username: &str) -> &'a Value {
    return rows
        .iter()
        .find(|row| row["username"].as_str() == Some(username))
        .expect("leaderboard row missing");
}

fn assert_pct(actual: &Value, expected: f64) {
    let value = actual.as_f64().expect("pct is not a number");
    assert!(
        (value - expected).abs() < 1e-9,
        "pct mismatch: expected {expected}, got {value}"
    );
}

fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("json object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    return keys;
}

#[tokio::test]
async fn weekdays_percent_exact_math() {
    let server = setup("weekdays-pct").await;
    let token = signup_user(&server, "pctwd", "UTC").await;
    let id = create_routine(&server, &token, weekdays_all_payload("daily stretch")).await;
    backdate_start(&server, id, past(40)).await;
    for offset in 1..=29u64 {
        if offset == 3 || offset == 20 {
            continue;
        }
        tick(&server, &token, id, past(offset)).await;
    }
    let rows = leaderboard_rows(&server, &token).await;
    let row = row_for(&rows, "pctwd");
    assert_pct(&row["pct_7d"], 5.0 / 6.0);
    assert_pct(&row["pct_30d"], 27.0 / 29.0);
    assert_eq!(row["streak_days"], 3);
    assert_eq!(row["pending_punishments"], 0);
}

#[tokio::test]
async fn count_percent_exact_math() {
    let server = setup("count-pct").await;
    let token = signup_user(&server, "pctcount", "UTC").await;
    let id = create_routine(&server, &token, count_payload("water", 1, "day")).await;
    backdate_start(&server, id, past(40)).await;
    tick(&server, &token, id, past(5)).await;
    let rows = leaderboard_rows(&server, &token).await;
    let row = row_for(&rows, "pctcount");
    assert_pct(&row["pct_7d"], 1.0 / 6.0);
    assert_pct(&row["pct_30d"], 1.0 / 29.0);
    assert_eq!(row["streak_days"], 1);
}

#[tokio::test]
async fn aggregate_kinds_and_mid_window_start() {
    let server = setup("aggregate").await;
    let token = signup_user(&server, "aggregator", "UTC").await;
    let weekdays_id =
        create_routine(&server, &token, weekdays_all_payload("aggregated stretch")).await;
    backdate_start(&server, weekdays_id, past(40)).await;
    for offset in 1..=29u64 {
        tick(&server, &token, weekdays_id, past(offset)).await;
    }
    let count_id =
        create_routine(&server, &token, count_payload("aggregated water", 1, "day")).await;
    backdate_start(&server, count_id, past(4)).await;
    tick(&server, &token, count_id, past(3)).await;
    tick(&server, &token, count_id, past(1)).await;
    let rows = leaderboard_rows(&server, &token).await;
    let row = row_for(&rows, "aggregator");
    assert_pct(&row["pct_7d"], 8.0 / 10.0);
    assert_pct(&row["pct_30d"], 31.0 / 33.0);
    assert_eq!(row["streak_days"], 2);
}

#[tokio::test]
async fn today_never_counts_against_you() {
    let server = setup("today-open").await;
    let token = signup_user(&server, "todayuser", "UTC").await;
    let id = create_routine(&server, &token, weekdays_all_payload("today stretch")).await;
    backdate_start(&server, id, past(10)).await;
    for offset in 1..=10u64 {
        tick(&server, &token, id, past(offset)).await;
    }
    let rows = leaderboard_rows(&server, &token).await;
    let row = row_for(&rows, "todayuser");
    assert_pct(&row["pct_7d"], 1.0);
    assert_pct(&row["pct_30d"], 1.0);
    assert_eq!(row["streak_days"], 10);
}

#[tokio::test]
async fn archived_routine_contributes_nothing() {
    let server = setup("archived-pct").await;
    let token = signup_user(&server, "archiver", "UTC").await;
    let id = create_routine(&server, &token, weekdays_all_payload("doomed routine")).await;
    backdate_start(&server, id, past(10)).await;
    let archived = json_call(
        &server,
        "POST",
        &format!("/api/v1/routines/{id}/archive"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(archived.status, 200, "archive failed: {archived:?}");
    let rows = leaderboard_rows(&server, &token).await;
    let row = row_for(&rows, "archiver");
    assert!(
        row["pct_7d"].is_null(),
        "archived misses must not create a %"
    );
    assert!(row["pct_30d"].is_null());
    assert!(row["streak_days"].is_null());
    assert_eq!(row["pending_punishments"], 0);
}

#[tokio::test]
async fn streak_miss_math() {
    let server = setup("streaks").await;

    let token = signup_user(&server, "missyesterday", "UTC").await;
    let id = create_routine(&server, &token, weekdays_all_payload("yd routine")).await;
    backdate_start(&server, id, past(10)).await;
    for offset in 2..=10u64 {
        tick(&server, &token, id, past(offset)).await;
    }
    let rows = leaderboard_rows(&server, &token).await;
    assert_eq!(row_for(&rows, "missyesterday")["streak_days"], 1);

    let token = signup_user(&server, "miss3ago", "UTC").await;
    let id = create_routine(&server, &token, weekdays_all_payload("d3 routine")).await;
    backdate_start(&server, id, past(10)).await;
    for offset in 1..=10u64 {
        if offset == 3 {
            continue;
        }
        tick(&server, &token, id, past(offset)).await;
    }
    let rows = leaderboard_rows(&server, &token).await;
    assert_eq!(row_for(&rows, "miss3ago")["streak_days"], 3);

    let token = signup_user(&server, "cleanstreak", "UTC").await;
    let id = create_routine(&server, &token, weekdays_all_payload("clean routine")).await;
    backdate_start(&server, id, past(10)).await;
    for offset in 1..=10u64 {
        tick(&server, &token, id, past(offset)).await;
    }
    let rows = leaderboard_rows(&server, &token).await;
    assert_eq!(row_for(&rows, "cleanstreak")["streak_days"], 10);

    let token = signup_user(&server, "countstreak", "UTC").await;
    let id = create_routine(&server, &token, count_payload("weekly quota", 1, "week")).await;
    let today = today_utc();
    let (ws, we) = week_bounds(today);
    backdate_start(&server, id, ws - Days::new(14)).await;
    tick(&server, &token, id, ws - Days::new(14)).await;
    tick(&server, &token, id, today).await;
    let expected = (today - (we - Days::new(7))).num_days();
    let rows = leaderboard_rows(&server, &token).await;
    assert_eq!(
        row_for(&rows, "countstreak")["streak_days"],
        expected,
        "count-kind miss must break streak at period_end"
    );
}

#[tokio::test]
async fn leaderboard_ordering_and_shame_counter() {
    let server = setup("ordering").await;

    let adam = signup_user(&server, "adam", "UTC").await;
    let adam_id = create_routine(&server, &adam, weekdays_all_payload("adam routine")).await;
    backdate_start(&server, adam_id, past(8)).await;
    for offset in 1..=8u64 {
        tick(&server, &adam, adam_id, past(offset)).await;
    }

    let zed = signup_user(&server, "zed", "UTC").await;
    let zed_id = create_routine(&server, &zed, weekdays_all_payload("zed routine")).await;
    backdate_start(&server, zed_id, past(8)).await;
    for offset in 1..=8u64 {
        tick(&server, &zed, zed_id, past(offset)).await;
    }

    let bob = signup_user(&server, "bob", "UTC").await;
    let bob_id = create_routine(&server, &bob, weekdays_all_payload("bob routine")).await;
    backdate_start(&server, bob_id, past(4)).await;
    for offset in 1..=4u64 {
        tick(&server, &bob, bob_id, past(offset)).await;
    }

    let carol = signup_user(&server, "carol", "UTC").await;
    let carol_id = create_routine(&server, &carol, weekdays_all_payload("carol routine")).await;
    backdate_start(&server, carol_id, past(8)).await;
    for offset in 5..=8u64 {
        tick(&server, &carol, carol_id, past(offset)).await;
    }

    let _dave = signup_user(&server, "dave", "UTC").await;
    let bob_user_id = user_id_by_name(&server, "bob").await;
    let dave_user_id = user_id_by_name(&server, "dave").await;
    add_punishments(&server, bob_user_id, "pending", 3).await;

    let rows = leaderboard_rows(&server, &adam).await;
    assert_eq!(rows.len(), 5);
    let order: Vec<&str> = rows
        .iter()
        .map(|row| row["username"].as_str().unwrap())
        .collect();
    assert_eq!(order, vec!["adam", "zed", "bob", "carol", "dave"]);
    let ranks: Vec<i64> = rows
        .iter()
        .map(|row| row["rank"].as_i64().unwrap())
        .collect();
    assert_eq!(ranks, vec![1, 2, 3, 4, 5]);
    assert_eq!(row_for(&rows, "bob")["pending_punishments"], 3);
    assert_eq!(row_for(&rows, "carol")["pending_punishments"], 0);
    assert_eq!(row_for(&rows, "carol")["streak_days"], 1);
    assert_pct(&row_for(&rows, "carol")["pct_7d"], 2.0 / 6.0);
    assert_pct(&row_for(&rows, "carol")["pct_30d"], 4.0 / 8.0);
    assert!(row_for(&rows, "dave")["pct_7d"].is_null());
    assert!(row_for(&rows, "dave")["streak_days"].is_null());

    let page = get_page(&server, "/leaderboard", &adam).await;
    assert_eq!(page.status, 200);
    assert!(page.body.contains("adam"));
    assert!(page.body.contains("dave"));
    assert!(
        page.body.contains('\u{2014}'),
        "null % must render as em dash"
    );
    assert!(
        page.body.contains(&format!("/users/{dave_user_id}")),
        "leaderboard rows must link to profiles"
    );
    assert!(
        page.body.contains(&format!(
            "/users/{}",
            user_id_by_name(&server, "adam").await
        )),
        "nav badge must link to own profile"
    );
}

#[tokio::test]
async fn profile_details_pin_cap_and_404() {
    let server = setup("profile").await;
    let token = signup_user(&server, "profiled", "UTC").await;
    let id = create_routine(&server, &token, weekdays_all_payload("profile routine")).await;
    backdate_start(&server, id, past(40)).await;
    let old_occurrence = tick(&server, &token, id, past(3)).await;
    let _mid = tick(&server, &token, id, past(2)).await;
    let new_occurrence = tick(&server, &token, id, past(1)).await;
    pin_occurrence(&server, &token, old_occurrence, "old pin").await;
    pin_occurrence(&server, &token, new_occurrence, "new pin").await;

    let user_id = user_id_by_name(&server, "profiled").await;
    sqlx::query("UPDATE users SET created_at = '2024-01-02T03:04:05+00:00' WHERE id = $1")
        .bind(user_id)
        .execute(&server.pool)
        .await
        .unwrap();
    add_punishments(&server, user_id, "pending", 1).await;
    add_punishments(&server, user_id, "cleared", 2).await;
    add_punishments(&server, user_id, "waived", 1).await;
    let (pin_id,): (i64,) = sqlx::query_as(
        "SELECT p.id FROM pins p JOIN completions c ON c.id = p.completion_id \
         WHERE c.occurrence_id = $1",
    )
    .bind(new_occurrence)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO attachments (pin_id, kind, url, file_path, created_at) \
         VALUES ($1, 'photo', NULL, 'profilethumb.png', $2)",
    )
    .bind(pin_id)
    .bind(Utc::now().to_rfc3339())
    .execute(&server.pool)
    .await
    .unwrap();

    let profile = profile_json(&server, &token, user_id).await;
    assert_eq!(profile["id"], user_id);
    assert_eq!(profile["username"], "profiled");
    assert_eq!(profile["timezone"], "UTC");
    assert_eq!(profile["member_since"], "2024-01-02");
    assert_pct(&profile["pct_7d"], 3.0 / 6.0);
    assert_pct(&profile["pct_30d"], 3.0 / 29.0);
    assert_eq!(profile["streak_days"], 4);
    assert_eq!(profile["punishments"]["pending"], 1);
    assert_eq!(profile["punishments"]["cleared"], 2);
    assert_eq!(profile["punishments"]["waived"], 1);
    assert_eq!(profile["routines"].as_array().unwrap().len(), 1);
    assert_eq!(
        profile["routines"][0]["description"],
        "Mon/Tue/Wed/Thu/Fri/Sat/Sun \u{b7} infinite"
    );
    assert_eq!(profile["routines"][0]["schedule"]["kind"], "weekdays");
    assert_eq!(profile["routines"][0]["schedule"]["weekdays_mask"], 127);
    assert_eq!(
        profile["routines"][0]["schedule"]["start_date"],
        past(40).to_string()
    );
    let pins = profile["pins"].as_array().unwrap();
    assert_eq!(pins.len(), 2);
    assert_eq!(pins[0]["summary"], "new pin");
    assert_eq!(pins[0]["due_date"], past(1).to_string());
    assert_eq!(pins[0]["photos"][0], "/uploads/profilethumb.png");
    assert_eq!(pins[1]["summary"], "old pin");

    let page = get_page(&server, &format!("/users/{user_id}"), &token).await;
    assert_eq!(page.status, 200);
    assert!(page.body.contains("profiled"));
    assert!(page.body.contains("2024-01-02"));
    assert!(
        page.body
            .contains("Mon/Tue/Wed/Thu/Fri/Sat/Sun \u{b7} infinite"),
        "schedule description missing: {}",
        page.body
    );
    assert!(page.body.contains("new pin"));
    assert!(
        page.body
            .contains(r#"<img src="/uploads/profilethumb.png""#)
    );
    assert!(page.body.contains(r#"href="/leaderboard""#));
    assert!(
        page.body.contains(&format!(r#"href="/users/{user_id}""#)),
        "nav badge must link to own profile"
    );

    let cap_token = signup_user(&server, "pincapped", "UTC").await;
    let cap_id = create_routine(&server, &cap_token, weekdays_all_payload("pin routine")).await;
    backdate_start(&server, cap_id, past(40)).await;
    let mut occurrences = Vec::new();
    for offset in 1..=12u64 {
        let occurrence_id = tick(&server, &cap_token, cap_id, past(offset)).await;
        occurrences.push((offset, occurrence_id));
    }
    for (offset, occurrence_id) in occurrences.iter().rev() {
        pin_occurrence(
            &server,
            &cap_token,
            *occurrence_id,
            &format!("pin {offset}"),
        )
        .await;
    }
    let cap_user_id = user_id_by_name(&server, "pincapped").await;
    let capped = profile_json(&server, &token, cap_user_id).await;
    let cap_pins = capped["pins"].as_array().unwrap();
    assert_eq!(cap_pins.len(), 10, "pins must cap at 10");
    assert_eq!(cap_pins[0]["summary"], "pin 1");
    assert_eq!(cap_pins[9]["summary"], "pin 10");

    let missing_api = request(
        server.addr,
        "GET",
        "/api/v1/users/999999",
        &[
            ("Cookie", format!("session={token}").as_str()),
            ("Content-Type", "application/json"),
        ],
        b"",
    )
    .await;
    assert_eq!(missing_api.status, 404);
    let missing_page = get_page(&server, "/users/999999", &token).await;
    assert_eq!(missing_page.status, 404);
}

#[test]
fn schedule_formatter_variants() {
    let today = NaiveDate::from_ymd_opt(2026, 9, 23).unwrap();
    let mon_wed_fri = Schedule {
        kind: ScheduleKind::Weekdays,
        weekdays_mask: mask_from_weekdays(&[Weekday::Mon, Weekday::Wed, Weekday::Fri]),
        period: PeriodKind::Week,
        per_period: 1,
        iterations: None,
        start_date: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
        end_date: None,
    };
    assert_eq!(
        describe_schedule(&mon_wed_fri, None, None, today),
        "Mon/Wed/Fri \u{b7} infinite"
    );
    let count_week = Schedule {
        kind: ScheduleKind::Count,
        weekdays_mask: 0,
        period: PeriodKind::Week,
        per_period: 2,
        iterations: Some(3),
        start_date: NaiveDate::from_ymd_opt(2026, 9, 21).unwrap(),
        end_date: None,
    };
    assert_eq!(
        describe_schedule(&count_week, None, None, today),
        "2\u{d7}/week \u{b7} 2 more periods"
    );
    let bounded = Schedule {
        kind: ScheduleKind::Weekdays,
        weekdays_mask: mask_from_weekdays(&[
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ]),
        period: PeriodKind::Week,
        per_period: 1,
        iterations: None,
        start_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        end_date: Some(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()),
    };
    assert_eq!(
        describe_schedule(&bounded, Some("18:00"), Some("22:00"), today),
        "Mon/Tue/Wed/Thu/Fri/Sat/Sun \u{b7} until 2026-10-01 \u{b7} 18:00\u{2013}22:00"
    );
    let count_month = Schedule {
        kind: ScheduleKind::Count,
        weekdays_mask: 0,
        period: PeriodKind::Month,
        per_period: 4,
        iterations: None,
        start_date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        end_date: None,
    };
    assert_eq!(
        describe_schedule(&count_month, Some("07:00"), None, today),
        "4\u{d7}/month \u{b7} infinite \u{b7} 07:00"
    );
    let weeks_bounded = Schedule {
        kind: ScheduleKind::Weekdays,
        weekdays_mask: mask_from_weekdays(&[Weekday::Mon, Weekday::Wed]),
        period: PeriodKind::Week,
        per_period: 1,
        iterations: Some(6),
        start_date: NaiveDate::from_ymd_opt(2026, 8, 3).unwrap(),
        end_date: None,
    };
    assert_eq!(
        describe_schedule(&weeks_bounded, None, None, today),
        "Mon/Wed \u{b7} 6 weeks"
    );
}

#[tokio::test]
async fn json_twins_shapes_and_unauth() {
    let server = setup("shapes").await;
    let token = signup_user(&server, "twin", "UTC").await;
    let id = create_routine(&server, &token, weekdays_all_payload("twin routine")).await;
    backdate_start(&server, id, past(8)).await;
    tick(&server, &token, id, past(1)).await;
    let user_id = user_id_by_name(&server, "twin").await;

    let rows = leaderboard_rows(&server, &token).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        sorted_keys(&rows[0]),
        vec![
            "pct_30d",
            "pct_7d",
            "pending_punishments",
            "rank",
            "streak_days",
            "user_id",
            "username"
        ]
    );
    assert_eq!(rows[0]["rank"], 1);

    let profile = profile_json(&server, &token, user_id).await;
    assert_eq!(
        sorted_keys(&profile),
        vec![
            "id",
            "member_since",
            "pct_30d",
            "pct_7d",
            "pins",
            "punishments",
            "routines",
            "streak_days",
            "timezone",
            "username"
        ]
    );
    assert_eq!(
        sorted_keys(&profile["routines"][0]["schedule"]),
        vec![
            "end_date",
            "iterations",
            "kind",
            "per_period",
            "period",
            "start_date",
            "weekdays_mask"
        ]
    );
    assert_eq!(
        sorted_keys(&profile["punishments"]),
        vec!["cleared", "pending", "waived"]
    );

    let unauth_page = request(server.addr, "GET", "/leaderboard", &[], b"").await;
    assert_eq!(unauth_page.status, 303);
    assert_eq!(unauth_page.header("location").as_deref(), Some("/login"));
    let unauth_api = request(server.addr, "GET", "/api/v1/leaderboard", &[], b"").await;
    assert_eq!(unauth_api.status, 401);
    let unauth_profile = request(server.addr, "GET", &format!("/users/{user_id}"), &[], b"").await;
    assert_eq!(unauth_profile.status, 303);
    assert_eq!(unauth_profile.header("location").as_deref(), Some("/login"));
    let unauth_profile_api = request(
        server.addr,
        "GET",
        &format!("/api/v1/users/{user_id}"),
        &[],
        b"",
    )
    .await;
    assert_eq!(unauth_profile_api.status, 401);
}

async fn seed_tick(pool: &SqlitePool, routine_id: i64, date: NaiveDate) -> i64 {
    let (user_id,): (i64,) = sqlx::query_as("SELECT user_id FROM routines WHERE id = $1")
        .bind(routine_id)
        .fetch_one(pool)
        .await
        .unwrap();
    let routine = routinely::store::routine_for_user(pool, user_id, routine_id)
        .await
        .unwrap()
        .unwrap();
    return routinely::store::tick(pool, &routine, date, None)
        .await
        .unwrap();
}
