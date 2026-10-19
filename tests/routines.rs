mod common;

use std::net::SocketAddr;
use std::time::Duration;

use chrono::Datelike;
use chrono::Days;
use chrono::NaiveDate;
use chrono::Utc;
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use routinely::app::AppState;
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
        "timezone": "UTC",
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
        &body.to_string(),
    )
    .await;
}

async fn form_post(server: &Server, path: &str, token: &str, body: &str) -> HttpResponse {
    let cookie = format!("session={token}");
    return request(
        server.addr,
        "POST",
        path,
        &[
            ("Cookie", cookie.as_str()),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        body,
    )
    .await;
}

async fn get_page(server: &Server, path: &str, token: &str) -> HttpResponse {
    let cookie = format!("session={token}");
    return request(server.addr, "GET", path, &[("Cookie", cookie.as_str())], "").await;
}

async fn create_routine(server: &Server, token: &str, payload: Value) -> i64 {
    let response = json_call(server, "POST", "/api/v1/routines", token, payload).await;
    assert_eq!(response.status, 200, "routine create failed: {response:?}");
    return response.json()["id"].as_i64().unwrap();
}

async fn update_routine(server: &Server, token: &str, id: i64, payload: Value) -> HttpResponse {
    return json_call(
        server,
        "PUT",
        &format!("/api/v1/routines/{id}"),
        token,
        payload,
    )
    .await;
}

async fn tick(server: &Server, token: &str, routine_id: i64, date: Option<NaiveDate>) {
    if let Some(day) = date.filter(|day| *day != routinely::store::user_today("UTC", 0)) {
        seed_tick(&server.pool, routine_id, day).await;
        return;
    }
    let path = match date {
        Some(day) => format!("/api/v1/occurrences/{routine_id}/completion?date={day}"),
        None => format!("/api/v1/occurrences/{routine_id}/completion"),
    };
    let response = json_call(server, "PUT", &path, token, serde_json::json!({})).await;
    assert_eq!(response.status, 200, "tick failed: {response:?}");
}

async fn backdate_start(server: &Server, routine_id: i64, date: NaiveDate) {
    sqlx::query("UPDATE routines SET start_date = $1 WHERE id = $2")
        .bind(date.to_string())
        .bind(routine_id)
        .execute(&server.pool)
        .await
        .unwrap();
}

async fn force_stale_eval(server: &Server, username: &str) {
    sqlx::query("UPDATE users SET last_evaluated_day = '2000-01-01' WHERE username = $1")
        .bind(username)
        .execute(&server.pool)
        .await
        .unwrap();
}

async fn scalar_count(server: &Server, sql: &'static str) -> i64 {
    let (value,): (i64,) = sqlx::query_as(sql).fetch_one(&server.pool).await.unwrap();
    return value;
}

fn today_utc() -> NaiveDate {
    return Utc::now().date_naive();
}

fn week_monday(today: NaiveDate) -> NaiveDate {
    let offset = today.weekday().num_days_from_monday();
    return today - Days::new(u64::from(offset));
}

fn mask(days: &[u8]) -> i64 {
    let mut mask = 0i64;
    for day in days {
        mask |= 1i64 << day;
    }
    return mask;
}

fn all_days() -> Vec<u8> {
    return vec![0, 1, 2, 3, 4, 5, 6];
}

fn all_except(index: u8) -> Vec<u8> {
    return (0..7).filter(|day| *day != index).collect();
}

#[tokio::test]
async fn non_schedule_edit_updates_in_place() {
    let server = setup("in-place").await;
    let token = signup_user(&server, "inplace").await;
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "gym",
            "kind": "weekdays",
            "weekdays": all_days(),
            "time_of_day": "07:00",
        }),
    )
    .await;
    let response = update_routine(
        &server,
        &token,
        id,
        serde_json::json!({
            "title": "gym renamed",
            "notes": "bring shoes",
            "kind": "weekdays",
            "weekdays": all_days(),
            "time_of_day": "08:30",
            "time_end": "09:00",
        }),
    )
    .await;
    assert_eq!(response.status, 200, "update failed: {response:?}");
    assert_eq!(response.json()["id"].as_i64().unwrap(), id);
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM routines")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "non-schedule edit must not create a row");
    let (title, notes, time_of_day, time_end, previous_id): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
    ) = sqlx::query_as(
        "SELECT title, notes, time_of_day, time_end, previous_id FROM routines WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(title, "gym renamed");
    assert_eq!(notes.as_deref(), Some("bring shoes"));
    assert_eq!(time_of_day.as_deref(), Some("08:30"));
    assert_eq!(time_end.as_deref(), Some("09:00"));
    assert!(
        previous_id.is_none(),
        "in-place edit must not set previous_id"
    );
}

#[tokio::test]
async fn weekday_split_mid_history_preserves_past_and_scores() {
    let server = setup("weekday-split").await;
    let token = signup_user(&server, "splitter").await;
    let today = today_utc();
    let yesterday = today - Days::new(1);
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "walk",
            "kind": "weekdays",
            "weekdays": all_days(),
        }),
    )
    .await;
    backdate_start(&server, id, today - Days::new(14)).await;
    force_stale_eval(&server, "splitter").await;
    let day = json_call(&server, "GET", "/api/v1/day", &token, serde_json::json!({})).await;
    assert_eq!(day.status, 200);
    let punishments_before = scalar_count(
        &server,
        "SELECT COUNT(*) FROM punishments WHERE source = 'auto' AND occurrence_id IS NOT NULL",
    )
    .await;
    assert!(
        punishments_before >= 1,
        "yesterday must have been evaluated as a miss"
    );
    let excluded = ((today.weekday().num_days_from_monday() + 1) % 7) as u8;
    let response = update_routine(
        &server,
        &token,
        id,
        serde_json::json!({
            "title": "walk",
            "kind": "weekdays",
            "weekdays": all_except(excluded),
        }),
    )
    .await;
    assert_eq!(response.status, 200, "split failed: {response:?}");
    let new_id = response.json()["id"].as_i64().unwrap();
    assert_ne!(new_id, id, "schedule edit mid-history must split");
    let (old_end, old_archived, old_start): (Option<String>, Option<String>, String) =
        sqlx::query_as("SELECT end_date, archived_at, start_date FROM routines WHERE id = $1")
            .bind(id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(old_end.as_deref(), Some(yesterday.to_string().as_str()));
    assert!(old_archived.is_none(), "old version must not be archived");
    assert_eq!(old_start, (today - Days::new(14)).to_string());
    let (new_start, new_previous, new_mask): (String, Option<i64>, i64) =
        sqlx::query_as("SELECT start_date, previous_id, weekdays_mask FROM routines WHERE id = $1")
            .bind(new_id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(new_start, today.to_string());
    assert_eq!(new_previous, Some(id));
    assert_eq!(new_mask, mask(&all_except(excluded)));
    let punishments_after = scalar_count(
        &server,
        "SELECT COUNT(*) FROM punishments WHERE source = 'auto' AND occurrence_id IS NOT NULL",
    )
    .await;
    assert_eq!(
        punishments_after, punishments_before,
        "past punishments must be untouched"
    );
    let (punished_routine,): (i64,) = sqlx::query_as(
        "SELECT o.routine_id FROM punishments p JOIN occurrences o ON o.id = p.occurrence_id \
         WHERE p.source = 'auto' AND p.occurrence_id IS NOT NULL LIMIT 1",
    )
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        punished_routine, id,
        "old occurrences stay on the old version"
    );
    let leaderboard = json_call(
        &server,
        "GET",
        "/api/v1/leaderboard",
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(leaderboard.status, 200);
    let rows = leaderboard.json();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["username"] == "splitter")
        .unwrap();
    assert!(
        !row["pct_7d"].is_null(),
        "scores must still count old history: {row}"
    );
}

#[tokio::test]
async fn ticked_and_pinned_today_moves_to_successor() {
    let server = setup("tick-pin-move").await;
    let token = signup_user(&server, "ticker").await;
    let today = today_utc();
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "meditate",
            "kind": "weekdays",
            "weekdays": all_days(),
        }),
    )
    .await;
    backdate_start(&server, id, today - Days::new(7)).await;
    tick(&server, &token, id, None).await;
    let day = json_call(&server, "GET", "/api/v1/day", &token, serde_json::json!({})).await;
    let occurrence_id = day.json()["routines"][0]["occurrence_id"].as_i64().unwrap();
    let pin = json_call(
        &server,
        "POST",
        &format!("/api/v1/occurrences/{occurrence_id}/pin"),
        &token,
        serde_json::json!({ "summary": "sat 20 minutes" }),
    )
    .await;
    assert_eq!(pin.status, 200, "pin failed: {pin:?}");
    let excluded = ((today.weekday().num_days_from_monday() + 1) % 7) as u8;
    let response = update_routine(
        &server,
        &token,
        id,
        serde_json::json!({
            "title": "meditate",
            "kind": "weekdays",
            "weekdays": all_except(excluded),
        }),
    )
    .await;
    assert_eq!(response.status, 200);
    let new_id = response.json()["id"].as_i64().unwrap();
    assert_ne!(new_id, id);
    let (moved_routine,): (i64,) =
        sqlx::query_as("SELECT routine_id FROM occurrences WHERE id = $1")
            .bind(occurrence_id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(
        moved_routine, new_id,
        "today's occurrence must be reparented"
    );
    let (pin_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM pins p \
         JOIN completions c ON c.id = p.completion_id \
         JOIN occurrences o ON o.id = c.occurrence_id WHERE o.id = $1",
    )
    .bind(occurrence_id)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(pin_count, 1, "pin must survive the reparent");
    let day = json_call(&server, "GET", "/api/v1/day", &token, serde_json::json!({})).await;
    let day_json = day.json();
    let entry = day_json["routines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_i64() == Some(new_id))
        .unwrap();
    assert_eq!(entry["completed"], true, "tick must show on successor");
}

#[tokio::test]
async fn excluded_today_drops_unticked_and_keeps_ticked() {
    let server = setup("excluded-today").await;
    let today = today_utc();
    let today_index = today.weekday().num_days_from_monday() as u8;
    let drop_token = signup_user(&server, "dropper").await;
    let drop_id = create_routine(
        &server,
        &drop_token,
        serde_json::json!({
            "title": "unticked",
            "kind": "weekdays",
            "weekdays": all_days(),
        }),
    )
    .await;
    backdate_start(&server, drop_id, today - Days::new(7)).await;
    let response = update_routine(
        &server,
        &drop_token,
        drop_id,
        serde_json::json!({
            "title": "unticked",
            "kind": "weekdays",
            "weekdays": all_except(today_index),
        }),
    )
    .await;
    assert_eq!(response.status, 200);
    let (today_occurrences,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM occurrences WHERE due_date = $1")
            .bind(today.to_string())
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(
        today_occurrences, 0,
        "unticked today occurrence must be dropped"
    );
    let keep_token = signup_user(&server, "keeper").await;
    let keep_id = create_routine(
        &server,
        &keep_token,
        serde_json::json!({
            "title": "ticked",
            "kind": "weekdays",
            "weekdays": all_days(),
        }),
    )
    .await;
    backdate_start(&server, keep_id, today - Days::new(7)).await;
    tick(&server, &keep_token, keep_id, None).await;
    let response = update_routine(
        &server,
        &keep_token,
        keep_id,
        serde_json::json!({
            "title": "ticked",
            "kind": "weekdays",
            "weekdays": all_except(today_index),
        }),
    )
    .await;
    assert_eq!(response.status, 200);
    let new_id = response.json()["id"].as_i64().unwrap();
    let (kept_routine, completion_count): (i64, i64) = sqlx::query_as(
        "SELECT o.routine_id, COUNT(c.id) FROM occurrences o \
         LEFT JOIN completions c ON c.occurrence_id = o.id \
         WHERE o.due_date = $1 GROUP BY o.routine_id",
    )
    .bind(today.to_string())
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        kept_routine, new_id,
        "ticked today occurrence moves to successor"
    );
    assert_eq!(completion_count, 1, "ticked today occurrence must be kept");
}

#[tokio::test]
async fn count_same_period_split_cuts_at_period_start_without_miss() {
    let server = setup("count-split").await;
    let token = signup_user(&server, "counter").await;
    let today = today_utc();
    let monday = week_monday(today);
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
    backdate_start(&server, id, monday - Days::new(7)).await;
    tick(&server, &token, id, Some(monday - Days::new(7))).await;
    tick(&server, &token, id, Some(monday - Days::new(6))).await;
    tick(&server, &token, id, Some(today)).await;
    let day = json_call(&server, "GET", "/api/v1/day", &token, serde_json::json!({})).await;
    assert_eq!(day.status, 200);
    let response = update_routine(
        &server,
        &token,
        id,
        serde_json::json!({
            "title": "water plants",
            "kind": "count",
            "per_period": 3,
            "period": "week",
        }),
    )
    .await;
    assert_eq!(response.status, 200, "count edit failed: {response:?}");
    let new_id = response.json()["id"].as_i64().unwrap();
    assert_ne!(new_id, id);
    let (new_start, new_per_period): (String, i64) =
        sqlx::query_as("SELECT start_date, per_period FROM routines WHERE id = $1")
            .bind(new_id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(
        new_start,
        monday.to_string(),
        "cutover must be the period start"
    );
    assert_eq!(new_per_period, 3);
    let (old_end,): (Option<String>,) =
        sqlx::query_as("SELECT end_date FROM routines WHERE id = $1")
            .bind(id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(
        old_end.as_deref(),
        Some((monday - Days::new(1)).to_string().as_str())
    );
    let (moved_routine, period_start): (i64, String) =
        sqlx::query_as("SELECT routine_id, period_start FROM occurrences WHERE due_date = $1")
            .bind(today.to_string())
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(moved_routine, new_id);
    assert_eq!(period_start, monday.to_string());
    let (progress_done,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM completions c JOIN occurrences o ON o.id = c.occurrence_id \
         WHERE o.routine_id = $1 AND o.period_start = $2",
    )
    .bind(new_id)
    .bind(monday.to_string())
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        progress_done, 1,
        "current period completions count on successor"
    );
    let old_reparented: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM occurrences WHERE routine_id = $1 AND due_date >= $2")
            .bind(id)
            .bind(monday.to_string())
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(
        old_reparented.0, 0,
        "no occurrences may remain on the old version from cutover on"
    );
    force_stale_eval(&server, "counter").await;
    let day = json_call(&server, "GET", "/api/v1/day", &token, serde_json::json!({})).await;
    assert_eq!(day.status, 200);
    let misses = scalar_count(&server, "SELECT COUNT(*) FROM period_misses").await;
    assert_eq!(
        misses, 0,
        "no miss for the old version's final period: {misses}"
    );
    let punishments = scalar_count(&server, "SELECT COUNT(*) FROM punishments").await;
    assert_eq!(punishments, 0, "no punishment may appear after the split");
}

#[tokio::test]
async fn count_to_weekdays_kind_change_no_spurious_miss() {
    let server = setup("kind-change").await;
    let token = signup_user(&server, "switcher").await;
    let today = today_utc();
    let monday = week_monday(today);
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "stretch",
            "kind": "count",
            "per_period": 2,
            "period": "week",
        }),
    )
    .await;
    backdate_start(&server, id, monday - Days::new(7)).await;
    tick(&server, &token, id, Some(monday - Days::new(7))).await;
    tick(&server, &token, id, Some(monday - Days::new(6))).await;
    tick(&server, &token, id, Some(today)).await;
    let day = json_call(&server, "GET", "/api/v1/day", &token, serde_json::json!({})).await;
    assert_eq!(day.status, 200);
    let response = update_routine(
        &server,
        &token,
        id,
        serde_json::json!({
            "title": "stretch",
            "kind": "weekdays",
            "weekdays": all_days(),
        }),
    )
    .await;
    assert_eq!(response.status, 200, "kind change failed: {response:?}");
    let new_id = response.json()["id"].as_i64().unwrap();
    let (new_kind, new_start): (String, String) =
        sqlx::query_as("SELECT kind, start_date FROM routines WHERE id = $1")
            .bind(new_id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(new_kind, "weekdays");
    assert_eq!(new_start, today.to_string(), "kind change cuts over today");
    let (old_end,): (Option<String>,) =
        sqlx::query_as("SELECT end_date FROM routines WHERE id = $1")
            .bind(id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(
        old_end.as_deref(),
        Some((monday - Days::new(1)).to_string().as_str()),
        "old count version must end before its partial current period"
    );
    let (today_period_start,): (Option<String>,) =
        sqlx::query_as("SELECT period_start FROM occurrences WHERE due_date = $1")
            .bind(today.to_string())
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert!(
        today_period_start.is_none(),
        "weekdays successor occurrences have no period"
    );
    let (past_on_old,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM occurrences WHERE routine_id = $1 AND due_date < $2")
            .bind(id)
            .bind(today.to_string())
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(
        past_on_old, 2,
        "previous week ticks stay on the old version"
    );
    force_stale_eval(&server, "switcher").await;
    let day = json_call(&server, "GET", "/api/v1/day", &token, serde_json::json!({})).await;
    assert_eq!(day.status, 200);
    let misses = scalar_count(&server, "SELECT COUNT(*) FROM period_misses").await;
    assert_eq!(
        misses, 0,
        "kind change must not create a spurious miss: {misses}"
    );
    let punishments = scalar_count(&server, "SELECT COUNT(*) FROM punishments").await;
    assert_eq!(punishments, 0);
}

#[tokio::test]
async fn non_head_and_foreign_updates_fail_and_starting_today_is_in_place() {
    let server = setup("guards").await;
    let alice = signup_user(&server, "aliceown").await;
    let bob = signup_user(&server, "bobforeign").await;
    let today = today_utc();
    let id = create_routine(
        &server,
        &alice,
        serde_json::json!({
            "title": "foreign target",
            "kind": "weekdays",
            "weekdays": all_days(),
        }),
    )
    .await;
    let foreign = update_routine(
        &server,
        &bob,
        id,
        serde_json::json!({
            "title": "hijack",
            "kind": "weekdays",
            "weekdays": all_days(),
        }),
    )
    .await;
    assert_eq!(foreign.status, 404, "foreign routine must 404");
    let missing = update_routine(
        &server,
        &alice,
        999_999,
        serde_json::json!({
            "title": "ghost",
            "kind": "weekdays",
            "weekdays": all_days(),
        }),
    )
    .await;
    assert_eq!(missing.status, 404, "missing routine must 404");
    backdate_start(&server, id, today - Days::new(7)).await;
    let split = update_routine(
        &server,
        &alice,
        id,
        serde_json::json!({
            "title": "foreign target",
            "kind": "weekdays",
            "weekdays": all_except(6),
        }),
    )
    .await;
    assert_eq!(split.status, 200);
    let successor = split.json()["id"].as_i64().unwrap();
    let stale = update_routine(
        &server,
        &alice,
        id,
        serde_json::json!({
            "title": "old head",
            "kind": "weekdays",
            "weekdays": all_days(),
        }),
    )
    .await;
    assert_eq!(stale.status, 409, "non-head routine must 409");
    let archived = json_call(
        &server,
        "POST",
        &format!("/api/v1/routines/{successor}/archive"),
        &alice,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(archived.status, 200);
    let archived_update = update_routine(
        &server,
        &alice,
        successor,
        serde_json::json!({
            "title": "archived head",
            "kind": "weekdays",
            "weekdays": all_days(),
        }),
    )
    .await;
    assert_eq!(archived_update.status, 409, "archived routine must 409");
    let fresh = create_routine(
        &server,
        &alice,
        serde_json::json!({
            "title": "starts today",
            "kind": "weekdays",
            "weekdays": all_days(),
        }),
    )
    .await;
    let in_place = update_routine(
        &server,
        &alice,
        fresh,
        serde_json::json!({
            "title": "starts today",
            "kind": "weekdays",
            "weekdays": all_except(6),
        }),
    )
    .await;
    assert_eq!(in_place.status, 200);
    assert_eq!(
        in_place.json()["id"].as_i64().unwrap(),
        fresh,
        "routine starting today edits in place"
    );
    let (rows, routine_start, routine_end): (i64, String, Option<String>) =
        sqlx::query_as("SELECT COUNT(*) OVER (), start_date, end_date FROM routines WHERE id = $1")
            .bind(fresh)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(rows, 1);
    assert_eq!(routine_start, today.to_string());
    assert!(
        routine_end.is_none(),
        "in-place schedule edit must not set end_date"
    );
    let (stored_mask,): (i64,) = sqlx::query_as("SELECT weekdays_mask FROM routines WHERE id = $1")
        .bind(fresh)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(stored_mask, mask(&all_except(6)));
}

#[tokio::test]
async fn ssr_pages_and_api_update_mirror() {
    let server = setup("ssr-mirror").await;
    let token = signup_user(&server, "ssruser").await;
    let created = form_post(
        &server,
        "/routines",
        &token,
        "title=Yoga&kind=weekdays&monday=on&tuesday=on",
    )
    .await;
    assert_eq!(created.status, 303, "SSR create failed: {created:?}");
    assert_eq!(created.header("location").as_deref(), Some("/routines"));
    let flash: Vec<String> = created
        .set_cookies()
        .into_iter()
        .filter(|cookie| cookie.starts_with("flash="))
        .collect();
    assert_eq!(flash.len(), 1);
    assert!(flash[0].contains("Routine%20created"));
    let (routine_id,): (i64,) = sqlx::query_as("SELECT id FROM routines WHERE title = 'Yoga'")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    let list = get_page(&server, "/routines", &token).await;
    assert_eq!(list.status, 200);
    assert!(
        list.body.contains("Yoga"),
        "routines page must list the routine"
    );
    assert!(
        list.body.contains("Active"),
        "routines page must have an Active section"
    );
    assert!(list.body.contains("New routine"));
    let edit = get_page(&server, &format!("/routines/{routine_id}/edit"), &token).await;
    assert_eq!(edit.status, 200);
    assert!(edit.body.contains("Edit routine"));
    assert!(edit.body.contains("Schedule changes apply from today"));
    assert!(edit.body.contains(r#"value="Yoga""#));
    assert!(edit.body.contains(r#"name="monday" checked"#));
    assert!(edit.body.contains("Cancel"));
    let api_update = update_routine(
        &server,
        &token,
        routine_id,
        serde_json::json!({
            "title": "Yoga API",
            "kind": "weekdays",
            "weekdays": vec![0, 1],
        }),
    )
    .await;
    assert_eq!(api_update.status, 200, "API update failed: {api_update:?}");
    assert_eq!(api_update.json()["id"].as_i64().unwrap(), routine_id);
    let (api_title,): (String,) = sqlx::query_as("SELECT title FROM routines WHERE id = $1")
        .bind(routine_id)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(api_title, "Yoga API");
    let monday = week_monday(today_utc());
    sqlx::query("UPDATE routines SET start_date = $1 WHERE id = $2")
        .bind((monday - Days::new(7)).to_string())
        .bind(routine_id)
        .execute(&server.pool)
        .await
        .unwrap();
    let ssr_update = form_post(
        &server,
        &format!("/routines/{routine_id}"),
        &token,
        "title=Yoga%20Split&kind=count&per_period=2&period=week",
    )
    .await;
    assert_eq!(ssr_update.status, 303, "SSR update failed: {ssr_update:?}");
    assert_eq!(ssr_update.header("location").as_deref(), Some("/routines"));
    let flash: Vec<String> = ssr_update
        .set_cookies()
        .into_iter()
        .filter(|cookie| cookie.starts_with("flash="))
        .collect();
    assert!(flash[0].contains("Routine%20updated"));
    let (head_id, previous_id): (i64, Option<i64>) =
        sqlx::query_as("SELECT id, previous_id FROM routines WHERE previous_id IS NOT NULL")
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(
        previous_id,
        Some(routine_id),
        "SSR update must split like the API"
    );
    assert_eq!(head_id, routine_id + 1);
    let archived = form_post(&server, &format!("/routines/{head_id}/archive"), &token, "").await;
    assert_eq!(archived.status, 303, "SSR archive failed: {archived:?}");
    assert_eq!(archived.header("location").as_deref(), Some("/routines"));
    let list = get_page(&server, "/routines", &token).await;
    assert!(list.body.contains("Archived"), "archived section missing");
    let invalid = form_post(
        &server,
        "/routines",
        &token,
        "title=&kind=weekdays&monday=on&wednesday=on",
    )
    .await;
    assert_eq!(invalid.status, 200, "invalid form must re-render");
    assert!(invalid.body.contains("field-error"), "field error missing");
    assert!(invalid.body.contains("form-error"), "summary error missing");
    assert!(
        invalid.body.contains(r#"name="monday" checked"#),
        "values must be preserved"
    );
    let foreign_edit = get_page(
        &server,
        &format!("/routines/{}/edit", routine_id + 99),
        &token,
    )
    .await;
    assert_eq!(foreign_edit.status, 404);
    let old_edit = get_page(&server, &format!("/routines/{routine_id}/edit"), &token).await;
    assert_eq!(
        old_edit.status, 303,
        "edit page for non-head must redirect to head"
    );
}

#[tokio::test]
async fn routines_page_splits_active_finished_and_hides_non_heads() {
    let server = setup("page-sections").await;
    let token = signup_user(&server, "lister").await;
    let today = today_utc();
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "finite",
            "kind": "weekdays",
            "weekdays": all_days(),
        }),
    )
    .await;
    backdate_start(&server, id, today - Days::new(30)).await;
    sqlx::query("UPDATE routines SET end_date = $1 WHERE id = $2")
        .bind((today - Days::new(1)).to_string())
        .bind(id)
        .execute(&server.pool)
        .await
        .unwrap();
    let _ = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "forever",
            "kind": "count",
            "per_period": 1,
            "period": "day",
        }),
    )
    .await;
    let list = get_page(&server, "/routines", &token).await;
    assert_eq!(list.status, 200);
    assert!(list.body.contains("Finished"), "finished section missing");
    assert!(list.body.contains("finite"));
    assert!(list.body.contains("forever"));
    assert!(list.body.contains("last 30 days"), "30d percent missing");
    let one_week = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "one week",
            "kind": "weekdays",
            "weekdays": all_days(),
            "iterations": 1,
        }),
    )
    .await;
    let archive = form_post(
        &server,
        &format!("/routines/{one_week}/archive"),
        &token,
        "",
    )
    .await;
    assert_eq!(archive.status, 303);
    let list = get_page(&server, "/routines", &token).await;
    assert!(
        list.body.contains(r#"class="archived""#),
        "archived details missing"
    );
    assert!(list.body.contains("one week"));
    let (archived_at,): (Option<String>,) =
        sqlx::query_as("SELECT archived_at FROM routines WHERE id = $1")
            .bind(one_week)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert!(archived_at.is_some());
}

#[tokio::test]
async fn ssr_create_renders_every_field_error_together() {
    let server = setup("ssr-all-errors").await;
    let token = signup_user(&server, "formerrors").await;
    let response = form_post(
        &server,
        "/routines",
        &token,
        "title=&kind=weekdays&duration=iterations&iterations=",
    )
    .await;
    assert_eq!(response.status, 200, "invalid form must re-render");
    assert!(
        response.body.contains("Please fix the highlighted fields."),
        "generic summary missing: {}",
        response.body
    );
    assert!(
        response.body.contains("Enter a title"),
        "title error missing: {}",
        response.body
    );
    assert!(
        response.body.contains("Pick at least one day"),
        "weekday error missing: {}",
        response.body
    );
    assert!(
        response.body.contains("Enter the number of weeks"),
        "iterations error missing: {}",
        response.body
    );
    assert!(
        response
            .body
            .contains(r#"aria-describedby="routine-iterations-error""#),
        "iterations error must be linked to its control"
    );
    assert!(
        response
            .body
            .contains(r#"aria-describedby="routine-weekdays-error""#),
        "weekday error must be linked to its controls"
    );
    assert!(
        response
            .body
            .contains(r#"name="duration" value="iterations" checked"#),
        "duration choice must be preserved"
    );
}

#[tokio::test]
async fn empty_routines_page_shows_empty_state() {
    let server = setup("empty-state").await;
    let token = signup_user(&server, "emptyuser").await;
    let list = get_page(&server, "/routines", &token).await;
    assert_eq!(list.status, 200);
    assert!(
        list.body.contains("No routines yet"),
        "empty state missing: {}",
        list.body
    );
    assert!(
        list.body.contains("/routines/new"),
        "empty state action missing"
    );
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

#[tokio::test]
async fn future_start_date_is_kept_and_validated() {
    let server = setup("future-start").await;
    let token = signup_user(&server, "ula").await;
    let start = today_utc() + Days::new(7);
    let id = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "run",
            "kind": "count",
            "per_period": 2,
            "period": "week",
            "start_date": start.to_string(),
        }),
    )
    .await;
    let (stored,): (String,) = sqlx::query_as("SELECT start_date FROM routines WHERE id = $1")
        .bind(id)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(stored, start.to_string());
    let past = json_call(
        &server,
        "POST",
        "/api/v1/routines",
        &token,
        serde_json::json!({
            "title": "late", "kind": "weekdays", "weekdays": [0],
            "start_date": (today_utc() - Days::new(1)).to_string(),
        }),
    )
    .await;
    assert_eq!(past.status, 422, "past start must be rejected: {past:?}");
    let inverted = json_call(
        &server,
        "POST",
        "/api/v1/routines",
        &token,
        serde_json::json!({
            "title": "inverted", "kind": "weekdays", "weekdays": [0],
            "start_date": start.to_string(),
            "end_date": (start - Days::new(1)).to_string(),
        }),
    )
    .await;
    assert_eq!(inverted.status, 422, "end before start must be rejected");
    let page = get_page(&server, "/routines", &token).await;
    assert!(
        page.body
            .contains(&format!("starts {}", start.format("%a %-d %b"))),
        "routines list must show the future start: {}",
        page.body
    );
    let form = get_page(&server, "/routines/new", &token).await;
    assert!(form.body.contains(r#"name="start_date""#));
    let edit = get_page(&server, &format!("/routines/{id}/edit"), &token).await;
    assert!(
        !edit.body.contains(r#"name="start_date""#),
        "edit must not offer start date"
    );
    let renamed = update_routine(
        &server,
        &token,
        id,
        serde_json::json!({ "title": "run far", "kind": "count", "per_period": 2, "period": "week" }),
    )
    .await;
    assert_eq!(renamed.status, 200, "rename failed: {renamed:?}");
    let (after,): (String,) =
        sqlx::query_as("SELECT start_date FROM routines WHERE title = 'run far'")
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(
        after,
        start.to_string(),
        "editing must keep the future start"
    );
    let ssr = form_post(
        &server,
        "/routines",
        &token,
        &format!("title=later&kind=weekdays&monday=on&duration=forever&start_date={start}"),
    )
    .await;
    assert_eq!(ssr.status, 303, "ssr create failed: {ssr:?}");
    let (ssr_start,): (String,) =
        sqlx::query_as("SELECT start_date FROM routines WHERE title = 'later'")
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(ssr_start, start.to_string());
}
