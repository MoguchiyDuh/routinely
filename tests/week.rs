mod common;

use std::net::SocketAddr;
use std::time::Duration;

use chrono::Datelike;
use chrono::Days;
use chrono::NaiveDate;
use chrono::TimeZone;
use chrono::Utc;
use chrono::Weekday;
use chrono_tz::America;
use chrono_tz::Europe;
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

async fn tick(server: &Server, token: &str, routine_id: i64, date: NaiveDate) {
    if date != routinely::store::user_today("UTC", 0) {
        seed_tick(&server.pool, routine_id, date).await;
        return;
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
}

async fn untick(server: &Server, token: &str, routine_id: i64, date: NaiveDate) {
    if date != routinely::store::user_today("UTC", 0) {
        routinely::store::untick(&server.pool, routine_id, date)
            .await
            .unwrap();
        return;
    }
    let response = json_call(
        server,
        "DELETE",
        &format!("/api/v1/occurrences/{routine_id}/completion?date={date}"),
        token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status, 200, "untick failed: {response:?}");
}

async fn pin_occurrence(
    server: &Server,
    token: &str,
    occurrence_id: i64,
    summary: &str,
    link: &str,
) {
    let response = json_call(
        server,
        "POST",
        &format!("/api/v1/occurrences/{occurrence_id}/pin"),
        token,
        serde_json::json!({ "summary": summary, "link_url": link }),
    )
    .await;
    assert_eq!(response.status, 200, "pin failed: {response:?}");
}

async fn week_json(server: &Server, token: &str, date: Option<NaiveDate>) -> Value {
    let path = match date {
        Some(day) => format!("/api/v1/week?date={day}"),
        None => "/api/v1/week".to_string(),
    };
    let response = json_call(server, "GET", &path, token, serde_json::json!({})).await;
    assert_eq!(response.status, 200, "week json failed: {response:?}");
    return response.json();
}

async fn week_json_who(server: &Server, token: &str, date: Option<NaiveDate>, who: &str) -> Value {
    let path = match date {
        Some(day) => format!("/api/v1/week?date={day}&who={who}"),
        None => format!("/api/v1/week?who={who}"),
    };
    let response = json_call(server, "GET", &path, token, serde_json::json!({})).await;
    assert_eq!(response.status, 200, "week json failed: {response:?}");
    return response.json();
}

async fn occurrence_id_for(server: &Server, token: &str, routine_id: i64, date: NaiveDate) -> i64 {
    let day = json_call(
        server,
        "GET",
        &format!("/api/v1/day?date={date}"),
        token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(day.status, 200, "day materialization call failed: {day:?}");
    let week = week_json(server, token, Some(date)).await;
    let entries = entries_for_day(&week, date);
    let entry = find_entry(&entries, "occurrence", routine_id).expect("occurrence entry missing");
    return entry["occurrence_id"].as_i64().expect("occurrence id");
}

async fn materialize_day(server: &Server, token: &str, date: NaiveDate) {
    let day = json_call(
        server,
        "GET",
        &format!("/api/v1/day?date={date}"),
        token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(day.status, 200, "day materialization call failed: {day:?}");
}

fn all_weekdays() -> Value {
    return serde_json::json!([0, 1, 2, 3, 4, 5, 6]);
}

fn weekdays_routine_payload(title: &str) -> Value {
    return serde_json::json!({
        "title": title,
        "kind": "weekdays",
        "weekdays": all_weekdays(),
    });
}

fn today_utc() -> NaiveDate {
    return Utc::now().date_naive();
}

fn week_bounds(date: NaiveDate) -> (NaiveDate, NaiveDate) {
    let offset = u64::from(date.weekday().num_days_from_monday());
    let start = date - Days::new(offset);
    return (start, start + Days::new(6));
}

fn entries_for_day(week: &Value, date: NaiveDate) -> Vec<Value> {
    let target = date.to_string();
    let Some(days) = week["days"].as_array() else {
        return Vec::new();
    };
    for day in days {
        if day["date"].as_str() == Some(target.as_str())
            && let Some(entries) = day["entries"].as_array()
        {
            return entries.clone();
        }
    }
    return Vec::new();
}

fn find_entry<'a>(entries: &'a [Value], kind: &str, routine_id: i64) -> Option<&'a Value> {
    return entries.iter().find(|entry| {
        entry["kind"].as_str() == Some(kind) && entry["routine_id"].as_i64() == Some(routine_id)
    });
}

fn find_todo<'a>(entries: &'a [Value], title: &str) -> Option<&'a Value> {
    return entries.iter().find(|entry| {
        entry["kind"].as_str() == Some("todo") && entry["title"].as_str() == Some(title)
    });
}

#[tokio::test]
async fn week_shows_completed_missed_and_future_planned() {
    let server = setup("statuses").await;
    let token = signup_user(&server, "weekuser", "UTC").await;
    let id = create_routine(
        &server,
        &token,
        weekdays_routine_payload("weekuser stretch"),
    )
    .await;
    let today = today_utc();
    let (week_start, _) = week_bounds(today);
    let prev_monday = week_start - Days::new(7);
    backdate_start(&server, id, prev_monday - Days::new(7)).await;
    materialize_day(&server, &token, today).await;
    let past_anchor = match today.weekday() {
        Weekday::Mon | Weekday::Tue => prev_monday,
        _ => week_start,
    };
    let past_completed = past_anchor;
    let past_missed = past_anchor + Days::new(1);
    tick(&server, &token, id, past_completed).await;
    tick(&server, &token, id, past_missed).await;
    untick(&server, &token, id, past_missed).await;
    let future_date = today + Days::new(1);
    let past_week = week_json(&server, &token, Some(past_anchor)).await;
    let completed_entries = entries_for_day(&past_week, past_completed);
    let completed = find_entry(&completed_entries, "occurrence", id)
        .expect("completed entry missing")
        .clone();
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["planned"], false);
    assert_eq!(completed["title"], "weekuser stretch");
    assert_eq!(completed["username"], "weekuser");
    assert!(completed["occurrence_id"].as_i64().is_some());
    let missed_entries = entries_for_day(&past_week, past_missed);
    let missed = find_entry(&missed_entries, "occurrence", id)
        .expect("missed entry missing")
        .clone();
    assert_eq!(missed["status"], "missed");
    let future_week = week_json(&server, &token, Some(future_date)).await;
    let future_entries = entries_for_day(&future_week, future_date);
    let planned = find_entry(&future_entries, "occurrence", id)
        .expect("planned entry missing")
        .clone();
    assert_eq!(planned["status"], "open");
    assert_eq!(planned["planned"], true);
    assert!(planned["occurrence_id"].is_null());
    let today_entries = entries_for_day(&future_week, today);
    if let Some(today_entry) = find_entry(&today_entries, "occurrence", id) {
        assert_eq!(
            today_entry["planned"], false,
            "today must come from the DB row, not the engine"
        );
    }
}

#[tokio::test]
async fn count_period_rows_open_and_closed() {
    let server = setup("count-periods").await;
    let token = signup_user(&server, "counter", "UTC").await;
    let today = today_utc();
    let (week_start, _) = week_bounds(today);
    let week_routine = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "water intake",
            "kind": "count",
            "per_period": 3,
            "period": "week",
        }),
    )
    .await;
    backdate_start(&server, week_routine, week_start - Days::new(7)).await;
    let ticked_on = if today == week_start {
        week_start + Days::new(1)
    } else {
        week_start
    };
    tick(&server, &token, week_routine, ticked_on).await;
    let daily_routine = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "one pushup",
            "kind": "count",
            "per_period": 1,
            "period": "day",
        }),
    )
    .await;
    backdate_start(&server, daily_routine, week_start - Days::new(14)).await;
    let closed_day = week_start - Days::new(3);
    tick(&server, &token, daily_routine, closed_day).await;
    let week = week_json(&server, &token, Some(week_start)).await;
    let mut period_rows: Vec<Value> = Vec::new();
    let mut offset = 0;
    while offset < 7 {
        let date = week_start + Days::new(offset);
        for entry in entries_for_day(&week, date) {
            if entry["kind"] == "period" && entry["routine_id"] == week_routine {
                period_rows.push(entry);
            }
        }
        offset += 1;
    }
    assert_eq!(
        period_rows.len(),
        1,
        "one open period row expected: {period_rows:?}"
    );
    assert_eq!(period_rows[0]["done"], 1);
    assert_eq!(period_rows[0]["required"], 3);
    assert_eq!(period_rows[0]["period_start"], week_start.to_string());
    let anchored = entries_for_day(&week, today);
    assert!(find_entry(&anchored, "period", week_routine).is_some());
    let past_week = week_json(&server, &token, Some(closed_day)).await;
    let closed_entries = entries_for_day(&past_week, closed_day);
    let closed_occ = find_entry(&closed_entries, "occurrence", daily_routine)
        .expect("closed day occurrence missing");
    assert_eq!(closed_occ["status"], "completed");
    assert!(
        find_entry(&closed_entries, "period", daily_routine).is_none(),
        "closed period must not render a period row: {closed_entries:?}"
    );
    let open_day = closed_day - Days::new(1);
    let open_entries = entries_for_day(&past_week, open_day);
    let open_period =
        find_entry(&open_entries, "period", daily_routine).expect("open period row missing");
    assert_eq!(open_period["done"], 0);
    assert_eq!(open_period["required"], 1);
    assert_eq!(open_period["period_start"], open_day.to_string());
}

#[tokio::test]
async fn todo_deadline_places_on_viewer_tz_day_with_overdue() {
    let server = setup("todo-tz").await;
    let berlin = signup_user(&server, "berlinuser", "Europe/Berlin").await;
    let ny = signup_user(&server, "nyviewer", "America/New_York").await;
    let (_, week_end) = week_bounds(today_utc());
    let next_monday = week_end + Days::new(1);
    let wednesday = next_monday + Days::new(2);
    let deadline_local = format!("{wednesday}T03:00");
    let created = json_call(
        &server,
        "POST",
        "/api/v1/todos",
        &berlin,
        serde_json::json!({ "title": "hand in report", "deadline": deadline_local }),
    )
    .await;
    assert_eq!(created.status, 200, "todo create failed: {created:?}");
    let berlin_week = week_json(&server, &berlin, Some(next_monday)).await;
    let berlin_day = entries_for_day(&berlin_week, wednesday);
    let berlin_todo = find_todo(&berlin_day, "hand in report").expect("berlin todo missing");
    assert_eq!(berlin_todo["local_time"], "03:00");
    assert_eq!(berlin_todo["username"], "berlinuser");
    assert_eq!(berlin_todo["overdue"], false);
    let ny_week = week_json(&server, &ny, Some(next_monday)).await;
    let ny_day_date = wednesday - Days::new(1);
    let ny_day = entries_for_day(&ny_week, ny_day_date);
    let ny_todo = find_todo(&ny_day, "hand in report").expect("ny todo missing");
    let naive = chrono::NaiveDateTime::parse_from_str(&deadline_local, "%Y-%m-%dT%H:%M").unwrap();
    let berlin_local = Europe::Berlin.from_local_datetime(&naive).single().unwrap();
    let ny_local = berlin_local.with_timezone(&America::New_York);
    assert_eq!(ny_todo["local_time"], ny_local.format("%H:%M").to_string());
    let berlin_today = Utc::now().with_timezone(&Europe::Berlin).date_naive();
    let overdue_created = json_call(
        &server,
        "POST",
        "/api/v1/todos",
        &berlin,
        serde_json::json!({
            "title": "pay fine",
            "deadline": format!("{berlin_today}T00:00"),
        }),
    )
    .await;
    assert_eq!(overdue_created.status, 200, "overdue todo create failed");
    let current = week_json(&server, &berlin, None).await;
    let today_entries = entries_for_day(&current, berlin_today);
    let overdue_todo = find_todo(&today_entries, "pay fine").expect("overdue todo missing");
    assert_eq!(overdue_todo["overdue"], true);
    let page = get_page(&server, "/week", &berlin).await;
    assert_eq!(page.status, 200);
    assert!(page.body.contains("pay fine"), "todo missing on SSR week");
    assert!(page.body.contains("overdue"), "overdue badge missing");
    let todo_id = overdue_todo["todo_id"].as_i64().unwrap();
    let toggled = json_call(
        &server,
        "POST",
        &format!("/api/v1/todos/{todo_id}/toggle"),
        &berlin,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(toggled.status, 200);
    let after = week_json(&server, &berlin, None).await;
    let after_entries = entries_for_day(&after, berlin_today);
    let done_todo = find_todo(&after_entries, "pay fine").expect("done todo missing");
    assert_eq!(done_todo["overdue"], false);
    assert_eq!(done_todo["completed"], true);
}

#[tokio::test]
async fn pin_fragment_serves_pin_and_404s_unpinned() {
    let server = setup("pin-fragment").await;
    let token = signup_user(&server, "pinner", "UTC").await;
    let id = create_routine(&server, &token, weekdays_routine_payload("pinned run")).await;
    let today = today_utc();
    let occurrence_id = occurrence_id_for(&server, &token, id, today).await;
    tick(&server, &token, id, today).await;
    pin_occurrence(
        &server,
        &token,
        occurrence_id,
        "morning run done",
        "https://example.com/route",
    )
    .await;
    let (pin_id,): (i64,) = sqlx::query_as(
        "SELECT p.id FROM pins p JOIN completions c ON c.id = p.completion_id \
         WHERE c.occurrence_id = $1",
    )
    .bind(occurrence_id)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO attachments (pin_id, kind, url, file_path, created_at) \
         VALUES ($1, 'photo', NULL, 'fragdigest.png', $2)",
    )
    .bind(pin_id)
    .bind(Utc::now().to_rfc3339())
    .execute(&server.pool)
    .await
    .unwrap();
    let cookie = format!("session={token}");
    let fragment = request(
        server.addr,
        "GET",
        &format!("/occurrences/{occurrence_id}/pin"),
        &[("Cookie", cookie.as_str())],
        b"",
    )
    .await;
    assert_eq!(fragment.status, 200, "pin fragment failed: {fragment:?}");
    assert!(fragment.body.contains("morning run done"));
    assert!(
        fragment
            .body
            .contains(r#"href="https://example.com/route""#)
    );
    assert!(
        fragment
            .body
            .contains(r#"<img src="/uploads/fragdigest.png""#)
    );
    assert!(
        fragment
            .body
            .contains(&format!(r#"id="pin-occ-{occurrence_id}""#))
    );
    let unpinned_routine =
        create_routine(&server, &token, weekdays_routine_payload("unpinned chore")).await;
    tick(&server, &token, unpinned_routine, today).await;
    let unpinned_oid = occurrence_id_for(&server, &token, unpinned_routine, today).await;
    let missing = request(
        server.addr,
        "GET",
        &format!("/occurrences/{unpinned_oid}/pin"),
        &[("Cookie", cookie.as_str())],
        b"",
    )
    .await;
    assert_eq!(missing.status, 404, "unpinned fragment must 404");
    let nonexistent = request(
        server.addr,
        "GET",
        "/occurrences/999999/pin",
        &[("Cookie", cookie.as_str())],
        b"",
    )
    .await;
    assert_eq!(nonexistent.status, 404);
    let unauth = request(
        server.addr,
        "GET",
        &format!("/occurrences/{occurrence_id}/pin"),
        &[],
        b"",
    )
    .await;
    assert_eq!(unauth.status, 303);
    assert_eq!(unauth.header("location").as_deref(), Some("/login"));
}

#[tokio::test]
async fn third_user_sees_all_group_activity() {
    let server = setup("group-visibility").await;
    let alice = signup_user(&server, "alice", "UTC").await;
    let bob = signup_user(&server, "bobby", "UTC").await;
    let carol = signup_user(&server, "carol", "UTC").await;
    let id = create_routine(&server, &alice, weekdays_routine_payload("alice stretch")).await;
    let today = today_utc();
    let occurrence_id = occurrence_id_for(&server, &alice, id, today).await;
    tick(&server, &alice, id, today).await;
    pin_occurrence(
        &server,
        &alice,
        occurrence_id,
        "alice pin evidence",
        "https://example.com/alice",
    )
    .await;
    let deadline_day = today + Days::new(3);
    let created = json_call(
        &server,
        "POST",
        "/api/v1/todos",
        &bob,
        serde_json::json!({
            "title": "bob chore",
            "deadline": format!("{deadline_day}T12:00"),
        }),
    )
    .await;
    assert_eq!(created.status, 200, "bob todo create failed");
    let week = week_json(&server, &carol, Some(today)).await;
    let today_entries = entries_for_day(&week, today);
    let alice_entry =
        find_entry(&today_entries, "occurrence", id).expect("carol must see alice occurrence");
    assert_eq!(alice_entry["username"], "alice");
    assert_eq!(alice_entry["pin"]["summary"], "alice pin evidence");
    let deadline_week = week_json(&server, &carol, Some(deadline_day)).await;
    let deadline_entries = entries_for_day(&deadline_week, deadline_day);
    let bob_todo = find_todo(&deadline_entries, "bob chore").expect("carol must see bob todo");
    assert_eq!(bob_todo["username"], "bobby");
    let page = get_page(&server, "/week?who=all", &carol).await;
    assert_eq!(page.status, 200);
    assert!(
        page.body.contains("alice stretch"),
        "carol SSR week must show alice routine"
    );
}

#[tokio::test]
async fn who_filter_scopes_week_entries() {
    let server = setup("who-filter").await;
    let alice = signup_user(&server, "alice2", "UTC").await;
    let bob = signup_user(&server, "bob2", "UTC").await;
    let alice_routine =
        create_routine(&server, &alice, weekdays_routine_payload("alice solo")).await;
    let bob_routine = create_routine(&server, &bob, weekdays_routine_payload("bob solo")).await;
    let today = today_utc();
    materialize_day(&server, &alice, today).await;
    materialize_day(&server, &bob, today).await;

    let default_week = week_json(&server, &alice, Some(today)).await;
    let default_entries = entries_for_day(&default_week, today);
    assert!(
        find_entry(&default_entries, "occurrence", alice_routine).is_some(),
        "api default must keep returning every user: {default_entries:?}"
    );
    assert!(find_entry(&default_entries, "occurrence", bob_routine).is_some());

    let me_week = week_json_who(&server, &alice, Some(today), "me").await;
    let me_entries = entries_for_day(&me_week, today);
    assert!(find_entry(&me_entries, "occurrence", alice_routine).is_some());
    assert!(
        find_entry(&me_entries, "occurrence", bob_routine).is_none(),
        "who=me must hide other users: {me_entries:?}"
    );

    let bob_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = 'bob2'")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    let bob_week = week_json_who(&server, &alice, Some(today), &bob_id.to_string()).await;
    let bob_entries = entries_for_day(&bob_week, today);
    assert!(find_entry(&bob_entries, "occurrence", bob_routine).is_some());
    assert!(
        find_entry(&bob_entries, "occurrence", alice_routine).is_none(),
        "who=bob must hide alice entries: {bob_entries:?}"
    );

    let api_unknown = json_call(
        &server,
        "GET",
        "/api/v1/week?who=424242",
        &alice,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(api_unknown.status, 404);

    let page = get_page(&server, "/week", &alice).await;
    assert_eq!(page.status, 200);
    assert!(
        page.body.contains("alice solo"),
        "own entry missing: {}",
        page.body
    );
    assert!(
        !page.body.contains("bob solo"),
        "default week page must show only own entries: {}",
        page.body
    );

    let all_page = get_page(&server, "/week?who=all", &alice).await;
    assert_eq!(all_page.status, 200);
    assert!(all_page.body.contains("alice solo"));
    assert!(
        all_page.body.contains("bob solo"),
        "all mode missing bob entry"
    );
    assert!(
        all_page.body.contains("bob2"),
        "all mode entries must carry owner name: {}",
        all_page.body
    );
    assert!(all_page.body.contains("Everyone"));

    let bob_page = get_page(&server, &format!("/week?who={bob_id}"), &alice).await;
    assert_eq!(bob_page.status, 200);
    assert!(bob_page.body.contains("bob solo"));
    assert!(
        !bob_page.body.contains("alice solo"),
        "who=bob page must hide alice entries: {}",
        bob_page.body
    );

    let unknown = get_page(&server, "/week?who=424242", &alice).await;
    assert_eq!(unknown.status, 404);

    let invalid = get_page(&server, "/week?who=bogus", &alice).await;
    assert_eq!(invalid.status, 200);
    assert!(invalid.body.contains("alice solo"));
    assert!(
        !invalid.body.contains("bob solo"),
        "invalid who must fall back to me: {}",
        invalid.body
    );
}

#[tokio::test]
async fn archived_routine_history_without_planned_rows() {
    let server = setup("archived-week").await;
    let token = signup_user(&server, "archiver", "UTC").await;
    let id = create_routine(&server, &token, weekdays_routine_payload("to archive")).await;
    let today = today_utc();
    let (week_start, week_end) = week_bounds(today);
    backdate_start(&server, id, week_start - Days::new(7)).await;
    tick(&server, &token, id, week_start).await;
    let archived = json_call(
        &server,
        "POST",
        &format!("/api/v1/routines/{id}/archive"),
        &token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(archived.status, 200, "archive failed");
    let week = week_json(&server, &token, Some(week_start)).await;
    let start_entries = entries_for_day(&week, week_start);
    let history =
        find_entry(&start_entries, "occurrence", id).expect("archived history entry missing");
    assert_eq!(history["status"], "completed");
    assert_eq!(history["planned"], false);
    let mut offset = 0;
    while offset < 7 {
        let date = week_start + Days::new(offset);
        if date > today {
            let entries = entries_for_day(&week, date);
            assert!(
                find_entry(&entries, "occurrence", id).is_none(),
                "archived routine must have no entries on future day {date}"
            );
        }
        offset += 1;
    }
    let next_monday = week_end + Days::new(1);
    let next_week = week_json(&server, &token, Some(next_monday)).await;
    let mut next_offset = 0;
    while next_offset < 7 {
        let date = next_monday + Days::new(next_offset);
        let entries = entries_for_day(&next_week, date);
        assert!(
            find_entry(&entries, "occurrence", id).is_none(),
            "archived routine must have no planned row on {date}"
        );
        next_offset += 1;
    }
}

#[tokio::test]
async fn week_bounds_cross_month_and_year_iso() {
    let server = setup("bounds").await;
    let token = signup_user(&server, "navigator", "UTC").await;
    let year_boundary = week_json(
        &server,
        &token,
        Some(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
    )
    .await;
    assert_eq!(year_boundary["week_start"], "2025-12-29");
    assert_eq!(year_boundary["week_end"], "2026-01-04");
    let days = year_boundary["days"].as_array().unwrap();
    assert_eq!(days.len(), 7);
    assert_eq!(days[0]["date"], "2025-12-29");
    assert_eq!(days[6]["date"], "2026-01-04");
    let month_boundary = week_json(
        &server,
        &token,
        Some(NaiveDate::from_ymd_opt(2026, 2, 1).unwrap()),
    )
    .await;
    assert_eq!(month_boundary["week_start"], "2026-01-26");
    assert_eq!(month_boundary["week_end"], "2026-02-01");
    let page = get_page(&server, "/week?date=2026-01-01", &token).await;
    assert_eq!(page.status, 200);
    assert!(page.body.contains("2025-12-29"), "week start missing");
    assert!(page.body.contains("2026-01-04"), "week end missing");
}

#[tokio::test]
async fn time_window_rendering_and_ordering() {
    let server = setup("ordering").await;
    let token = signup_user(&server, "scheduler", "UTC").await;
    let early = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "aaa early",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
            "time_of_day": "07:30",
            "time_end": "08:00",
        }),
    )
    .await;
    let mid = create_routine(
        &server,
        &token,
        serde_json::json!({
            "title": "bbb mid",
            "kind": "weekdays",
            "weekdays": all_weekdays(),
            "time_of_day": "09:00",
        }),
    )
    .await;
    let untimed = create_routine(&server, &token, weekdays_routine_payload("ccc untimed")).await;
    let today = today_utc();
    let week = week_json(&server, &token, Some(today)).await;
    let entries = entries_for_day(&week, today);
    let order: Vec<i64> = entries
        .iter()
        .filter(|entry| entry["kind"] == "occurrence")
        .map(|entry| entry["routine_id"].as_i64().unwrap())
        .collect();
    assert_eq!(order, vec![early, mid, untimed], "wrong day cell order");
    let page = get_page(&server, "/week", &token).await;
    assert_eq!(page.status, 200);
    assert!(
        page.body.contains("07:30\u{2013}08:00"),
        "time window missing: {}",
        page.body
    );
}

#[tokio::test]
async fn api_week_twin_shape() {
    let server = setup("api-twin").await;
    let token = signup_user(&server, "twinuser", "UTC").await;
    let id = create_routine(&server, &token, weekdays_routine_payload("twin walk")).await;
    let today = today_utc();
    let occurrence_id = occurrence_id_for(&server, &token, id, today).await;
    tick(&server, &token, id, today).await;
    pin_occurrence(
        &server,
        &token,
        occurrence_id,
        "wall sit done",
        "https://example.com/wall",
    )
    .await;
    let (pin_id,): (i64,) = sqlx::query_as(
        "SELECT p.id FROM pins p JOIN completions c ON c.id = p.completion_id \
         WHERE c.occurrence_id = $1",
    )
    .bind(occurrence_id)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO attachments (pin_id, kind, url, file_path, created_at) \
         VALUES ($1, 'photo', NULL, 'twindigest.png', $2)",
    )
    .bind(pin_id)
    .bind(Utc::now().to_rfc3339())
    .execute(&server.pool)
    .await
    .unwrap();
    let week = week_json(&server, &token, Some(today)).await;
    assert!(week["week_start"].as_str().is_some());
    assert!(week["week_end"].as_str().is_some());
    let entries = entries_for_day(&week, today);
    let entry = find_entry(&entries, "occurrence", id).expect("twin entry missing");
    assert_eq!(entry["kind"], "occurrence");
    assert_eq!(entry["username"], "twinuser");
    assert_eq!(entry["status"], "completed");
    assert_eq!(entry["pin"]["summary"], "wall sit done");
    assert_eq!(entry["pin"]["links"][0], "https://example.com/wall");
    assert_eq!(entry["pin"]["photos"][0], "/uploads/twindigest.png");
}

#[tokio::test]
async fn unauth_week_requests_redirect_or_401() {
    let server = setup("unauth-week").await;
    let page = request(server.addr, "GET", "/week", &[], b"").await;
    assert_eq!(page.status, 303);
    assert_eq!(page.header("location").as_deref(), Some("/login"));
    let api = request(server.addr, "GET", "/api/v1/week", &[], b"").await;
    assert_eq!(api.status, 401);
    let fragment = request(server.addr, "GET", "/occurrences/1/pin", &[], b"").await;
    assert_eq!(fragment.status, 303);
    assert_eq!(fragment.header("location").as_deref(), Some("/login"));
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
async fn week_grid_offers_ticks_only_for_today_and_pins_goals() {
    let server = setup("week-grid").await;
    let token = signup_user(&server, "vic", "UTC").await;
    let today = Utc::now().date_naive();
    let week_start = today - Days::new(u64::from(today.weekday().num_days_from_monday()));
    let daily = create_routine(
        &server,
        &token,
        serde_json::json!({ "title": "floss", "kind": "weekdays", "weekdays": all_weekdays(), "time_of_day": "08:00" }),
    )
    .await;
    backdate_start(&server, daily, week_start - Days::new(7)).await;
    let goal = create_routine(
        &server,
        &token,
        serde_json::json!({ "title": "lift", "kind": "count", "per_period": 3, "period": "week" }),
    )
    .await;
    backdate_start(&server, goal, week_start - Days::new(7)).await;
    tick(&server, &token, goal, today).await;
    let page = get_page(&server, &format!("/week?date={today}"), &token).await;
    assert_eq!(page.status, 200);
    assert!(page.body.contains("wk-grid"), "desktop grid missing");
    assert!(page.body.contains("goal-lane"), "goal lane missing");
    assert!(
        page.body
            .contains(&format!("/occurrences/{goal}/untick?date={today}"))
    );
    for (index, _) in page.body.match_indices("/tick?date=") {
        let date = &page.body[index + 11..index + 21];
        assert_eq!(date, today.to_string(), "tick form offered for {date}");
    }
    for (index, _) in page.body.match_indices("/untick?date=") {
        let date = &page.body[index + 13..index + 23];
        assert_eq!(date, today.to_string(), "untick form offered for {date}");
    }
    assert!(
        page.body
            .contains(&format!("/occurrences/{daily}/tick?date={today}"))
    );
    assert!(
        !page.body.contains("1/3 this week</span>"),
        "weekly goals must not be repeated as day entries"
    );
    let past = get_page(
        &server,
        &format!("/week?date={}", week_start - Days::new(7)),
        &token,
    )
    .await;
    assert!(
        !past.body.contains("/tick?date="),
        "past week must be read-only"
    );
}
