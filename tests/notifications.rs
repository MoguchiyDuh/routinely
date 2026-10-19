mod common;

use std::net::SocketAddr;
use std::time::Duration;

use chrono::DateTime;
use chrono::NaiveDate;
use chrono::NaiveTime;
use chrono::Utc;
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use routinely::app::AppState;
use routinely::db;
use routinely::notify;
use routinely::store;
use routinely::store::ReminderError;
use routinely::store::ReminderSpec;
use routinely::store::RoutineSpec;

const SIGNUP_CODE: &str = "test-code";
const PASSWORD: &str = "password123";

async fn pool() -> SqlitePool {
    let config = common::config(SIGNUP_CODE, Duration::from_secs(600)).await;
    return db::init(&config).await.unwrap();
}

async fn make_user(pool: &SqlitePool, name: &str, timezone: &str) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO users (username, password_hash, timezone, created_at) VALUES ($1, 'x', $2, $3) RETURNING id",
    )
    .bind(name)
    .bind(timezone)
    .bind(Utc::now().to_rfc3339())
    .fetch_one(pool)
    .await
    .unwrap();
    return id;
}

fn at(raw: &str) -> DateTime<Utc> {
    return DateTime::parse_from_rfc3339(raw)
        .unwrap()
        .with_timezone(&Utc);
}

fn day(raw: &str) -> NaiveDate {
    return NaiveDate::parse_from_str(raw, "%Y-%m-%d").unwrap();
}

fn spec(value: i64, unit: &str, at_time: Option<&str>) -> ReminderSpec {
    return ReminderSpec {
        offset_value: value,
        offset_unit: unit.to_string(),
        at_time: at_time.map(str::to_string),
    };
}

fn routine_spec(title: &str, time_of_day: Option<&str>, start: &str) -> RoutineSpec {
    return RoutineSpec {
        title: title.to_string(),
        notes: None,
        kind: store::ScheduleKind::Weekdays,
        weekdays_mask: Some(127),
        period: None,
        per_period: None,
        iterations: None,
        end_date: None,
        time_of_day: time_of_day.map(str::to_string),
        time_end: None,
        start_date: day(start),
    };
}

async fn notifications(pool: &SqlitePool, user_id: i64) -> Vec<notify::NotificationRow> {
    return notify::list(pool, user_id, 100, None).await.unwrap();
}

#[tokio::test]
async fn timed_routine_fires_ten_minutes_before_in_user_timezone() {
    let pool = pool().await;
    let user = make_user(&pool, "alice", "Europe/Berlin").await;
    let routine = store::create_routine(
        &pool,
        user,
        &routine_spec("Run", Some("18:00"), "2030-03-01"),
    )
    .await
    .unwrap();
    store::replace_reminders(
        &pool,
        user,
        "routine",
        routine,
        &[spec(10, "minutes", None)],
    )
    .await
    .unwrap();
    notify::run_tick(&pool, at("2030-03-04T16:49:00Z")).await;
    assert!(notifications(&pool, user).await.is_empty());
    notify::run_tick(&pool, at("2030-03-04T16:55:00Z")).await;
    let items = notifications(&pool, user).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "Run");
    assert_eq!(items[0].body.as_deref(), Some("Starts in 10 minutes"));
    assert_eq!(items[0].url.as_deref(), Some("/day?date=2030-03-04"));
    assert_eq!(items[0].kind, "routine_reminder");
    assert!(items[0].unread);
}

#[tokio::test]
async fn all_day_routine_fires_one_day_before_at_chosen_time() {
    let pool = pool().await;
    let user = make_user(&pool, "bob", "UTC").await;
    let routine = store::create_routine(
        &pool,
        user,
        &routine_spec("Water plants", None, "2030-03-01"),
    )
    .await
    .unwrap();
    store::replace_reminders(
        &pool,
        user,
        "routine",
        routine,
        &[spec(1, "days", Some("09:00"))],
    )
    .await
    .unwrap();
    notify::run_tick(&pool, at("2030-03-04T08:59:00Z")).await;
    assert!(notifications(&pool, user).await.is_empty());
    notify::run_tick(&pool, at("2030-03-04T09:05:00Z")).await;
    let items = notifications(&pool, user).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].body.as_deref(), Some("Tomorrow"));
    assert_eq!(items[0].url.as_deref(), Some("/day?date=2030-03-05"));
}

#[tokio::test]
async fn todo_deadline_fires_three_days_before() {
    let pool = pool().await;
    let user = make_user(&pool, "carol", "UTC").await;
    let todo = store::create_todo(
        &pool,
        user,
        "File taxes",
        None,
        Some("2030-03-07T23:59:59+00:00"),
        "deadline",
        true,
    )
    .await
    .unwrap();
    store::replace_reminders(&pool, user, "todo", todo, &[spec(3, "days", Some("09:00"))])
        .await
        .unwrap();
    notify::run_tick(&pool, at("2030-03-04T09:02:00Z")).await;
    let items = notifications(&pool, user).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "File taxes");
    assert_eq!(items[0].body.as_deref(), Some("Deadline in 3 days"));
    assert_eq!(items[0].kind, "todo_reminder");
    store::toggle_todo(&pool, user, todo).await.unwrap();
    notify::run_tick(&pool, at("2030-03-04T09:03:00Z")).await;
    assert_eq!(notifications(&pool, user).await.len(), 1);
}

#[tokio::test]
async fn timed_todo_fires_relative_to_deadline() {
    let pool = pool().await;
    let user = make_user(&pool, "dave", "UTC").await;
    let todo = store::create_todo(
        &pool,
        user,
        "Call bank",
        None,
        Some("2030-03-04T15:00:00+00:00"),
        "deadline",
        false,
    )
    .await
    .unwrap();
    store::replace_reminders(&pool, user, "todo", todo, &[spec(1, "hours", None)])
        .await
        .unwrap();
    notify::run_tick(&pool, at("2030-03-04T14:01:00Z")).await;
    let items = notifications(&pool, user).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].body.as_deref(), Some("Deadline in 1 hour"));
}

#[tokio::test]
async fn second_tick_is_idempotent() {
    let pool = pool().await;
    let user = make_user(&pool, "erin", "UTC").await;
    let routine = store::create_routine(
        &pool,
        user,
        &routine_spec("Stretch", Some("08:00"), "2030-03-01"),
    )
    .await
    .unwrap();
    store::replace_reminders(&pool, user, "routine", routine, &[spec(0, "minutes", None)])
        .await
        .unwrap();
    notify::run_tick(&pool, at("2030-03-04T08:00:30Z")).await;
    notify::run_tick(&pool, at("2030-03-04T08:01:00Z")).await;
    notify::run_tick(&pool, at("2030-03-04T08:10:00Z")).await;
    let items = notifications(&pool, user).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].body.as_deref(), Some("Starts now"));
}

#[tokio::test]
async fn late_ticks_and_late_reminders_do_not_fire_retroactively() {
    let pool = pool().await;
    let user = make_user(&pool, "frank", "UTC").await;
    let routine = store::create_routine(
        &pool,
        user,
        &routine_spec("Read", Some("18:00"), "2030-03-01"),
    )
    .await
    .unwrap();
    store::replace_reminders(
        &pool,
        user,
        "routine",
        routine,
        &[spec(10, "minutes", None)],
    )
    .await
    .unwrap();
    notify::run_tick(&pool, at("2030-03-04T17:30:00Z")).await;
    assert!(notifications(&pool, user).await.is_empty());
    sqlx::query("UPDATE reminders SET created_at = '2030-03-04T17:52:00+00:00'")
        .execute(&pool)
        .await
        .unwrap();
    notify::run_tick(&pool, at("2030-03-04T17:55:00Z")).await;
    assert!(notifications(&pool, user).await.is_empty());
    sqlx::query("UPDATE reminders SET created_at = '2030-03-04T17:40:00+00:00'")
        .execute(&pool)
        .await
        .unwrap();
    notify::run_tick(&pool, at("2030-03-04T17:55:00Z")).await;
    assert_eq!(notifications(&pool, user).await.len(), 1);
}

#[tokio::test]
async fn completed_and_archived_routines_are_skipped() {
    let pool = pool().await;
    let user = make_user(&pool, "gina", "UTC").await;
    let done = store::create_routine(
        &pool,
        user,
        &routine_spec("Done", Some("18:00"), "2030-03-01"),
    )
    .await
    .unwrap();
    let archived = store::create_routine(
        &pool,
        user,
        &routine_spec("Gone", Some("18:00"), "2030-03-01"),
    )
    .await
    .unwrap();
    for id in [done, archived] {
        store::replace_reminders(&pool, user, "routine", id, &[spec(10, "minutes", None)])
            .await
            .unwrap();
    }
    let row = store::routine_for_user(&pool, user, done)
        .await
        .unwrap()
        .unwrap();
    store::tick(&pool, &row, day("2030-03-04"), None)
        .await
        .unwrap();
    store::archive_routine(&pool, user, archived).await.unwrap();
    notify::run_tick(&pool, at("2030-03-04T17:55:00Z")).await;
    assert!(notifications(&pool, user).await.is_empty());
}

#[tokio::test]
async fn count_routine_skips_when_period_goal_is_met() {
    let pool = pool().await;
    let user = make_user(&pool, "hank", "UTC").await;
    let routine = store::create_routine(
        &pool,
        user,
        &RoutineSpec {
            title: "Gym".to_string(),
            notes: None,
            kind: store::ScheduleKind::Count,
            weekdays_mask: None,
            period: Some(store::PeriodKind::Week),
            per_period: Some(1),
            iterations: None,
            end_date: None,
            time_of_day: None,
            time_end: None,
            start_date: day("2030-03-01"),
        },
    )
    .await
    .unwrap();
    store::replace_reminders(
        &pool,
        user,
        "routine",
        routine,
        &[spec(0, "days", Some("12:00"))],
    )
    .await
    .unwrap();
    notify::run_tick(&pool, at("2030-03-04T12:01:00Z")).await;
    assert_eq!(notifications(&pool, user).await.len(), 1);
    let row = store::routine_for_user(&pool, user, routine)
        .await
        .unwrap()
        .unwrap();
    store::tick(&pool, &row, day("2030-03-05"), None)
        .await
        .unwrap();
    notify::run_tick(&pool, at("2030-03-06T12:01:00Z")).await;
    assert_eq!(notifications(&pool, user).await.len(), 1);
}

#[tokio::test]
async fn master_switch_off_silences_everything() {
    let pool = pool().await;
    let user = make_user(&pool, "ivan", "UTC").await;
    let owner = make_user(&pool, "ivan_owner", "UTC").await;
    routinely::punish::seed_catalog_if_empty(&pool, owner)
        .await
        .unwrap();
    let routine = store::create_routine(
        &pool,
        user,
        &routine_spec("Plain", Some("18:00"), "2030-03-01"),
    )
    .await
    .unwrap();
    store::replace_reminders(
        &pool,
        user,
        "routine",
        routine,
        &[spec(10, "minutes", None)],
    )
    .await
    .unwrap();
    let todo = store::create_todo(
        &pool,
        user,
        "Task",
        None,
        Some("2030-03-04T18:00:00+00:00"),
        "deadline",
        false,
    )
    .await
    .unwrap();
    store::replace_reminders(&pool, user, "todo", todo, &[spec(5, "minutes", None)])
        .await
        .unwrap();
    let prefs = notify::NotificationPrefs {
        enabled: false,
        ..notify::NotificationPrefs::default()
    };
    notify::save_prefs(&pool, user, &prefs).await.unwrap();
    for stamp in [
        "2030-03-04T17:50:00Z",
        "2030-03-04T17:56:00Z",
        "2030-03-04T22:30:00Z",
        "2030-03-05T00:10:00Z",
    ] {
        notify::run_tick(&pool, at(stamp)).await;
    }
    assert!(notifications(&pool, user).await.is_empty());
    notify::run_tick(&pool, Utc::now()).await;
    assert!(notifications(&pool, user).await.is_empty());
    assert_eq!(notify::unread_count(&pool, user).await.unwrap(), 0);
}

#[tokio::test]
async fn absent_prefs_row_resolves_to_defaults() {
    let pool = pool().await;
    let user = make_user(&pool, "ivy", "UTC").await;
    let prefs = notify::get_prefs(&pool, user).await.unwrap();
    assert_eq!(prefs, notify::NotificationPrefs::default());
    assert!(prefs.enabled && prefs.day_end_enabled && prefs.penalty_enabled);
    assert_eq!(prefs.day_end_time, "20:00");
    assert_eq!(prefs.default_routine_timed_minutes, Some(10));
    assert!(!prefs.default_routine_allday_enabled);
    assert_eq!(prefs.default_todo_timed_minutes, Some(60));
    assert_eq!(prefs.default_todo_allday_days, Some(1));
    notify::run_tick(&pool, at("2030-03-04T12:00:00Z")).await;
    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notification_prefs WHERE user_id = $1 AND notifications_since IS NOT NULL")
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn default_specs_follow_prefs() {
    let prefs = notify::NotificationPrefs::default();
    assert_eq!(
        store::default_reminder_specs(&prefs, "routine", true),
        vec![spec(10, "minutes", None)]
    );
    assert!(store::default_reminder_specs(&prefs, "routine", false).is_empty());
    assert_eq!(
        store::default_reminder_specs(&prefs, "todo", true),
        vec![spec(60, "minutes", None)]
    );
    assert_eq!(
        store::default_reminder_specs(&prefs, "todo", false),
        vec![spec(1, "days", Some("09:00"))]
    );
    let allday = notify::NotificationPrefs {
        default_routine_allday_enabled: true,
        default_routine_allday_time: "07:30".to_string(),
        default_todo_timed_minutes: None,
        ..prefs.clone()
    };
    assert_eq!(
        store::default_reminder_specs(&allday, "routine", false),
        vec![spec(0, "days", Some("07:30"))]
    );
    assert!(store::default_reminder_specs(&allday, "todo", true).is_empty());
    let off = notify::NotificationPrefs {
        enabled: false,
        ..prefs
    };
    assert!(store::default_reminder_specs(&off, "routine", true).is_empty());
}

#[test]
fn prefs_validation_ranges() {
    let base = notify::NotificationPrefs::default();
    let bad = |prefs: notify::NotificationPrefs| assert!(notify::validate_prefs(&prefs).is_err());
    bad(notify::NotificationPrefs {
        default_routine_timed_minutes: Some(40321),
        ..base.clone()
    });
    bad(notify::NotificationPrefs {
        default_todo_timed_minutes: Some(-1),
        ..base.clone()
    });
    bad(notify::NotificationPrefs {
        default_todo_allday_days: Some(29),
        ..base.clone()
    });
    bad(notify::NotificationPrefs {
        default_todo_allday_time: "9am".to_string(),
        ..base.clone()
    });
    bad(notify::NotificationPrefs {
        default_routine_allday_time: "25:00".to_string(),
        ..base.clone()
    });
    bad(notify::NotificationPrefs {
        day_end_time: "x".to_string(),
        ..base.clone()
    });
    let ok = notify::NotificationPrefs {
        default_routine_timed_minutes: None,
        default_todo_allday_days: Some(0),
        default_todo_allday_time: "9:05".to_string(),
        ..base
    };
    let checked = notify::validate_prefs(&ok).unwrap();
    assert_eq!(checked.default_todo_allday_time, "09:05");
    assert_eq!(checked.default_routine_timed_minutes, None);
}

#[tokio::test]
async fn day_end_nudge_defaults_on_and_fires_once_per_day() {
    let pool = pool().await;
    let user = make_user(&pool, "judy", "UTC").await;
    let routine = store::create_routine(&pool, user, &routine_spec("Journal", None, "2030-03-01"))
        .await
        .unwrap();
    notify::run_tick(&pool, at("2030-03-04T19:59:00Z")).await;
    assert!(notifications(&pool, user).await.is_empty());
    notify::run_tick(&pool, at("2030-03-04T20:00:00Z")).await;
    notify::run_tick(&pool, at("2030-03-04T22:30:00Z")).await;
    let items = notifications(&pool, user).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, "day_end");
    assert_eq!(items[0].body.as_deref(), Some("Journal"));
    let row = store::routine_for_user(&pool, user, routine)
        .await
        .unwrap()
        .unwrap();
    store::tick(&pool, &row, day("2030-03-05"), None)
        .await
        .unwrap();
    notify::run_tick(&pool, at("2030-03-05T22:30:00Z")).await;
    assert_eq!(notifications(&pool, user).await.len(), 1);
    let quiet_user = make_user(&pool, "judy_off", "UTC").await;
    store::create_routine(
        &pool,
        quiet_user,
        &routine_spec("Journal", None, "2030-03-01"),
    )
    .await
    .unwrap();
    let prefs = notify::NotificationPrefs {
        day_end_enabled: false,
        ..notify::NotificationPrefs::default()
    };
    notify::save_prefs(&pool, quiet_user, &prefs).await.unwrap();
    notify::run_tick(&pool, at("2030-03-04T22:30:00Z")).await;
    assert!(notifications(&pool, quiet_user).await.is_empty());
}

#[tokio::test]
async fn quiet_hours_defer_day_end_and_drop_reminders() {
    let pool = pool().await;
    let user = make_user(&pool, "kate", "UTC").await;
    let routine = store::create_routine(
        &pool,
        user,
        &routine_spec("Late", Some("22:10"), "2030-03-01"),
    )
    .await
    .unwrap();
    store::replace_reminders(&pool, user, "routine", routine, &[spec(5, "minutes", None)])
        .await
        .unwrap();
    let prefs = notify::NotificationPrefs {
        day_end_enabled: true,
        quiet_start: Some("22:00".to_string()),
        quiet_end: Some("23:00".to_string()),
        ..notify::NotificationPrefs::default()
    };
    notify::save_prefs(&pool, user, &notify::validate_prefs(&prefs).unwrap())
        .await
        .unwrap();
    notify::run_tick(&pool, at("2030-03-04T22:06:00Z")).await;
    assert!(notifications(&pool, user).await.is_empty());
    notify::run_tick(&pool, at("2030-03-04T23:01:00Z")).await;
    let items = notifications(&pool, user).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, "day_end");
}

#[test]
fn quiet_window_wraps_midnight() {
    let prefs = notify::NotificationPrefs {
        quiet_start: Some("23:00".to_string()),
        quiet_end: Some("07:00".to_string()),
        ..notify::NotificationPrefs::default()
    };
    let time = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
    assert!(notify::in_quiet_hours(&prefs, time(23, 30)));
    assert!(notify::in_quiet_hours(&prefs, time(2, 0)));
    assert!(!notify::in_quiet_hours(&prefs, time(7, 0)));
    assert!(!notify::in_quiet_hours(&prefs, time(12, 0)));
    assert!(!notify::in_quiet_hours(
        &notify::NotificationPrefs::default(),
        time(2, 0)
    ));
}

#[tokio::test]
async fn penalty_events_default_on_without_flooding_history() {
    let pool = pool().await;
    let user = make_user(&pool, "leo", "UTC").await;
    let owner = make_user(&pool, "mia", "UTC").await;
    routinely::punish::seed_catalog_if_empty(&pool, owner)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO punishments (user_id, custom_title, source, status, assigned_at) VALUES ($1, 'ancient', 'manual', 'pending', '2020-01-01T00:00:00+00:00')",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    let today = store::user_today("UTC", 0);
    let start = today.checked_sub_days(chrono::Days::new(3)).unwrap();
    store::create_routine(
        &pool,
        user,
        &routine_spec("Daily", None, &start.to_string()),
    )
    .await
    .unwrap();
    let prefs = notify::NotificationPrefs {
        day_end_enabled: false,
        ..notify::NotificationPrefs::default()
    };
    notify::save_prefs(&pool, user, &prefs).await.unwrap();
    notify::run_tick(&pool, Utc::now()).await;
    let issued = notifications(&pool, user).await;
    let (auto,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM punishments WHERE user_id = $1 AND source = 'auto'")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(auto > 0);
    assert_eq!(i64::try_from(issued.len()).unwrap(), auto);
    assert!(
        issued
            .iter()
            .all(|item| item.kind == "penalty_issued" && item.body.as_deref() != Some("ancient"))
    );
    let (first,): (i64,) =
        sqlx::query_as("SELECT MIN(id) FROM punishments WHERE user_id = $1 AND source = 'auto'")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    routinely::punish::close_punishment(&pool, user, first, true)
        .await
        .unwrap();
    notify::run_tick(&pool, Utc::now()).await;
    let after = notifications(&pool, user).await;
    assert_eq!(after.len(), issued.len() + 1);
    assert!(after.iter().any(|item| item.title == "Penalty waived"));
    let disabled = make_user(&pool, "leo_off", "UTC").await;
    store::create_routine(
        &pool,
        disabled,
        &routine_spec("Daily", None, &start.to_string()),
    )
    .await
    .unwrap();
    let off = notify::NotificationPrefs {
        penalty_enabled: false,
        day_end_enabled: false,
        ..notify::NotificationPrefs::default()
    };
    notify::save_prefs(&pool, disabled, &off).await.unwrap();
    notify::run_tick(&pool, Utc::now()).await;
    assert!(notifications(&pool, disabled).await.is_empty());
}

#[tokio::test]
async fn deleting_targets_and_users_removes_reminders() {
    let pool = pool().await;
    let user = make_user(&pool, "nina", "UTC").await;
    let routine = store::create_routine(
        &pool,
        user,
        &routine_spec("Tmp", Some("10:00"), "2030-03-01"),
    )
    .await
    .unwrap();
    let todo = store::create_todo(
        &pool,
        user,
        "T",
        None,
        Some("2030-03-09T10:00:00+00:00"),
        "deadline",
        false,
    )
    .await
    .unwrap();
    store::replace_reminders(&pool, user, "routine", routine, &[spec(5, "minutes", None)])
        .await
        .unwrap();
    store::replace_reminders(&pool, user, "todo", todo, &[spec(5, "minutes", None)])
        .await
        .unwrap();
    let count = || async {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM reminders")
            .fetch_one(&pool)
            .await
            .unwrap();
        return n;
    };
    assert_eq!(count().await, 2);
    store::delete_todo(&pool, user, todo).await.unwrap();
    assert_eq!(count().await, 1);
    store::archive_routine(&pool, user, routine).await.unwrap();
    assert_eq!(count().await, 1);
    store::delete_archived_routine(&pool, user, routine)
        .await
        .unwrap();
    assert_eq!(count().await, 0);
    let other = store::create_routine(&pool, user, &routine_spec("Again", None, "2030-03-01"))
        .await
        .unwrap();
    store::replace_reminders(
        &pool,
        user,
        "routine",
        other,
        &[spec(0, "days", Some("08:00"))],
    )
    .await
    .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count().await, 0);
}

#[tokio::test]
async fn reminder_validation_rules() {
    let pool = pool().await;
    let user = make_user(&pool, "omar", "UTC").await;
    let timed = store::create_routine(
        &pool,
        user,
        &routine_spec("Timed", Some("10:00"), "2030-03-01"),
    )
    .await
    .unwrap();
    let untimed = store::create_routine(&pool, user, &routine_spec("Untimed", None, "2030-03-01"))
        .await
        .unwrap();
    let no_deadline = store::create_todo(&pool, user, "Loose", None, None, "todo", false)
        .await
        .unwrap();
    let invalid = |result: Result<(), ReminderError>| {
        assert!(
            matches!(result, Err(ReminderError::Invalid(_))),
            "{result:?}"
        );
    };
    invalid(
        store::replace_reminders(&pool, user, "routine", timed, &[spec(1, "days", None)]).await,
    );
    invalid(
        store::replace_reminders(
            &pool,
            user,
            "routine",
            timed,
            &[spec(10, "minutes", Some("09:00"))],
        )
        .await,
    );
    invalid(
        store::replace_reminders(
            &pool,
            user,
            "routine",
            untimed,
            &[spec(10, "minutes", None)],
        )
        .await,
    );
    invalid(
        store::replace_reminders(&pool, user, "routine", untimed, &[spec(1, "weeks", None)]).await,
    );
    invalid(
        store::replace_reminders(
            &pool,
            user,
            "routine",
            timed,
            &[spec(1, "days", Some("25:00"))],
        )
        .await,
    );
    invalid(
        store::replace_reminders(
            &pool,
            user,
            "routine",
            timed,
            &[spec(1, "years", Some("09:00"))],
        )
        .await,
    );
    invalid(
        store::replace_reminders(
            &pool,
            user,
            "routine",
            timed,
            &[spec(-1, "days", Some("09:00"))],
        )
        .await,
    );
    invalid(
        store::replace_reminders(
            &pool,
            user,
            "routine",
            timed,
            &[spec(99, "weeks", Some("09:00"))],
        )
        .await,
    );
    invalid(
        store::replace_reminders(
            &pool,
            user,
            "todo",
            no_deadline,
            &[spec(1, "days", Some("09:00"))],
        )
        .await,
    );
    let six: Vec<ReminderSpec> = (0..6).map(|n| spec(n, "minutes", None)).collect();
    invalid(store::replace_reminders(&pool, user, "routine", timed, &six).await);
    store::replace_reminders(
        &pool,
        user,
        "routine",
        timed,
        &[
            spec(10, "minutes", None),
            spec(10, "minutes", None),
            spec(1, "days", Some("9:05")),
        ],
    )
    .await
    .unwrap();
    let rows = store::list_reminders_for(&pool, user, "routine", &[timed, untimed])
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].at_time.as_deref(), Some("09:05"));
    let five: Vec<ReminderSpec> = (0..5).map(|n| spec(n, "minutes", None)).collect();
    store::replace_reminders(&pool, user, "routine", timed, &five)
        .await
        .unwrap();
    assert!(matches!(
        store::add_reminder(&pool, user, "routine", timed, &spec(9, "minutes", None)).await,
        Err(ReminderError::Invalid(_))
    ));
    store::replace_reminders(&pool, user, "routine", timed, &[spec(3, "minutes", None)])
        .await
        .unwrap();
    assert!(matches!(
        store::add_reminder(&pool, user, "routine", timed, &spec(3, "minutes", None)).await,
        Err(ReminderError::Duplicate)
    ));
    let stranger = make_user(&pool, "pia", "UTC").await;
    assert!(matches!(
        store::replace_reminders(&pool, stranger, "routine", timed, &[]).await,
        Err(ReminderError::NotFound)
    ));
}

#[tokio::test]
async fn editing_schedule_carries_reminders_to_new_chain_row() {
    let pool = pool().await;
    let user = make_user(&pool, "quinn", "UTC").await;
    let today = store::user_today("UTC", 0);
    let routine = store::create_routine(
        &pool,
        user,
        &routine_spec("Chain", Some("10:00"), "2020-01-01"),
    )
    .await
    .unwrap();
    store::replace_reminders(
        &pool,
        user,
        "routine",
        routine,
        &[spec(10, "minutes", None)],
    )
    .await
    .unwrap();
    let mut changed = routine_spec("Chain", Some("10:00"), "2020-01-01");
    changed.weekdays_mask = Some(31);
    let head = store::update_routine(&pool, user, routine, &changed, today)
        .await
        .unwrap();
    assert_ne!(head, routine);
    assert!(
        store::list_reminders_for(&pool, user, "routine", &[routine])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store::list_reminders_for(&pool, user, "routine", &[head])
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn purge_removes_old_notifications() {
    let pool = pool().await;
    let user = make_user(&pool, "ruth", "UTC").await;
    let now = at("2030-06-01T00:00:00Z");
    for (key, created, read) in [
        ("old-read", "2030-04-01T00:00:00+00:00", true),
        ("recent-read", "2030-05-25T00:00:00+00:00", true),
        ("mid-unread", "2030-04-01T00:00:00+00:00", false),
        ("old-unread", "2030-01-01T00:00:00+00:00", false),
    ] {
        sqlx::query(
            "INSERT INTO notifications (user_id, kind, title, dedupe_key, created_at, read_at) VALUES ($1, 'x', $2, $2, $3, $4)",
        )
        .bind(user)
        .bind(key)
        .bind(created)
        .bind(read.then_some(created))
        .execute(&pool)
        .await
        .unwrap();
    }
    assert_eq!(notify::purge_old(&pool, now).await.unwrap(), 2);
    let titles: Vec<String> = notifications(&pool, user)
        .await
        .into_iter()
        .map(|n| n.title)
        .collect();
    assert_eq!(
        titles,
        vec!["mid-unread".to_string(), "recent-read".to_string()]
    );
}

struct Server {
    addr: SocketAddr,
    pool: SqlitePool,
}

async fn setup() -> Server {
    let config = common::config(SIGNUP_CODE, Duration::from_secs(600)).await;
    let pool = db::init(&config).await.unwrap();
    let state = AppState {
        pool: pool.clone(),
        config,
    };
    let router = routinely::app::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    return Server { addr, pool };
}

async fn call(
    server: &Server,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (u16, Value) {
    let mut stream = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    let payload = body.to_string();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\nContent-Type: application/json\r\n"
    );
    if let Some(token) = token {
        req.push_str(&format!("Cookie: session={token}\r\n"));
    }
    req.push_str(&format!(
        "Content-Length: {}\r\n\r\n{payload}",
        payload.len()
    ));
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    let json = serde_json::from_str(&body).unwrap_or(Value::Null);
    let cookie = text
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: session="))
        .and_then(|rest| rest.split(';').next())
        .map(str::to_string);
    return (
        status,
        match cookie {
            Some(token) => serde_json::json!({ "token": token, "body": json }),
            None => json,
        },
    );
}

async fn signup(server: &Server, name: &str) -> String {
    let (status, value) = call(
        server,
        "POST",
        "/api/v1/auth/signup",
        None,
        serde_json::json!({ "username": name, "password": PASSWORD, "code": SIGNUP_CODE, "timezone": "UTC" }),
    )
    .await;
    assert_eq!(status, 200);
    return value["token"].as_str().unwrap().to_string();
}

#[tokio::test]
async fn api_reminders_notifications_and_prefs() {
    let server = setup().await;
    let alice = signup(&server, "alice_api").await;
    let bob = signup(&server, "bob_api").await;
    let (status, created) = call(
        &server,
        "POST",
        "/api/v1/routines",
        Some(&alice),
        serde_json::json!({
            "title": "Run", "kind": "weekdays", "weekdays": [0, 1, 2, 3, 4, 5, 6],
            "time_of_day": "18:00",
            "reminders": [{ "offset_value": 10, "offset_unit": "minutes" }, { "offset_value": 1, "offset_unit": "days", "at_time": "09:00" }]
        }),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    let routine_id = created["id"].as_i64().unwrap();
    let (status, bad) = call(
        &server,
        "POST",
        "/api/v1/routines",
        Some(&alice),
        serde_json::json!({
            "title": "Bad", "kind": "weekdays", "weekdays": [0],
            "reminders": [{ "offset_value": 1, "offset_unit": "days" }]
        }),
    )
    .await;
    assert_eq!(status, 422, "{bad}");
    let (status, listed) = call(
        &server,
        "GET",
        &format!("/api/v1/reminders?target_type=routine&target_id={routine_id}"),
        Some(&alice),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(listed["items"].as_array().unwrap().len(), 2);
    let (_, bobs) = call(&server, "GET", "/api/v1/reminders", Some(&bob), Value::Null).await;
    assert!(bobs["items"].as_array().unwrap().is_empty());
    let first = listed["items"][0]["id"].as_i64().unwrap();
    let (status, _) = call(
        &server,
        "DELETE",
        &format!("/api/v1/reminders/{first}"),
        Some(&bob),
        Value::Null,
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = call(
        &server,
        "DELETE",
        &format!("/api/v1/reminders/{first}"),
        Some(&alice),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = call(
        &server,
        "POST",
        "/api/v1/reminders",
        Some(&alice),
        serde_json::json!({ "target_type": "routine", "target_id": routine_id, "offset_value": 5, "offset_unit": "minutes" }),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = call(
        &server,
        "POST",
        "/api/v1/reminders",
        Some(&bob),
        serde_json::json!({ "target_type": "routine", "target_id": routine_id, "offset_value": 5, "offset_unit": "minutes" }),
    )
    .await;
    assert_eq!(status, 404);

    let alice_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = 'alice_api'")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    for key in ["a", "b"] {
        sqlx::query(
            "INSERT INTO notifications (user_id, kind, title, dedupe_key, created_at) VALUES ($1, 'x', $2, $2, $3)",
        )
        .bind(alice_id)
        .bind(key)
        .bind(Utc::now().to_rfc3339())
        .execute(&server.pool)
        .await
        .unwrap();
    }
    let (_, unread) = call(
        &server,
        "GET",
        "/api/v1/notifications/unread_count",
        Some(&alice),
        Value::Null,
    )
    .await;
    assert_eq!(unread["count"], 2);
    let (_, bob_unread) = call(
        &server,
        "GET",
        "/api/v1/notifications/unread_count",
        Some(&bob),
        Value::Null,
    )
    .await;
    assert_eq!(bob_unread["count"], 0);
    let (_, page) = call(
        &server,
        "GET",
        "/api/v1/notifications?limit=1",
        Some(&alice),
        Value::Null,
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["unread"], true);
    let newest = page["items"][0]["id"].as_i64().unwrap();
    assert_eq!(page["next_before_id"].as_i64().unwrap(), newest);
    let (_, older) = call(
        &server,
        "GET",
        &format!("/api/v1/notifications?limit=5&before_id={newest}"),
        Some(&alice),
        Value::Null,
    )
    .await;
    assert_eq!(older["items"].as_array().unwrap().len(), 1);
    let (status, _) = call(
        &server,
        "POST",
        &format!("/api/v1/notifications/{newest}/read"),
        Some(&bob),
        Value::Null,
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = call(
        &server,
        "POST",
        &format!("/api/v1/notifications/{newest}/read"),
        Some(&alice),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    let (_, unread) = call(
        &server,
        "GET",
        "/api/v1/notifications/unread_count",
        Some(&alice),
        Value::Null,
    )
    .await;
    assert_eq!(unread["count"], 1);
    let (status, _) = call(
        &server,
        "POST",
        "/api/v1/notifications/read",
        Some(&alice),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    let (_, unread) = call(
        &server,
        "GET",
        "/api/v1/notifications/unread_count",
        Some(&alice),
        Value::Null,
    )
    .await;
    assert_eq!(unread["count"], 0);

    let (_, prefs) = call(
        &server,
        "GET",
        "/api/v1/notifications/prefs",
        Some(&alice),
        Value::Null,
    )
    .await;
    assert_eq!(prefs["enabled"], true);
    assert_eq!(prefs["day_end_enabled"], true);
    assert_eq!(prefs["day_end_time"], "20:00");
    assert_eq!(prefs["default_routine_timed_minutes"], 10);
    assert_eq!(prefs["default_todo_allday_days"], 1);
    let (status, saved) = call(
        &server,
        "PUT",
        "/api/v1/notifications/prefs",
        Some(&alice),
        serde_json::json!({ "day_end_time": "21:30", "quiet_start": "23:00", "quiet_end": "07:00", "default_routine_timed_minutes": null, "default_todo_allday_days": 2 }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(saved["day_end_time"], "21:30");
    assert_eq!(saved["default_routine_timed_minutes"], Value::Null);
    let (_, reread) = call(
        &server,
        "GET",
        "/api/v1/notifications/prefs",
        Some(&alice),
        Value::Null,
    )
    .await;
    assert_eq!(reread, saved);
    let (status, _) = call(
        &server,
        "PUT",
        "/api/v1/notifications/prefs",
        Some(&alice),
        serde_json::json!({ "default_todo_allday_days": 99 }),
    )
    .await;
    assert_eq!(status, 422);
    let (status, _) = call(
        &server,
        "PUT",
        "/api/v1/notifications/prefs",
        Some(&alice),
        serde_json::json!({ "quiet_start": "23:00" }),
    )
    .await;
    assert_eq!(status, 422);
    let (status, _) = call(&server, "GET", "/api/v1/notifications", None, Value::Null).await;
    assert_eq!(status, 401);
}

async fn reminders_of(server: &Server, token: &str, target_type: &str, id: i64) -> Vec<Value> {
    let (status, listed) = call(
        server,
        "GET",
        &format!("/api/v1/reminders?target_type={target_type}&target_id={id}"),
        Some(token),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    return listed["items"].as_array().unwrap().clone();
}

async fn user_id(server: &Server, name: &str) -> i64 {
    return sqlx::query_scalar("SELECT id FROM users WHERE username = $1")
        .bind(name)
        .fetch_one(&server.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn json_create_applies_defaults_unless_reminders_given() {
    let server = setup().await;
    let token = signup(&server, "defaults_user").await;
    let routine = |extra: Value| {
        let mut body = serde_json::json!({ "title": "R", "kind": "weekdays", "weekdays": [0, 1, 2, 3, 4, 5, 6] });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        return body;
    };
    let (_, timed) = call(
        &server,
        "POST",
        "/api/v1/routines",
        Some(&token),
        routine(serde_json::json!({ "time_of_day": "18:00" })),
    )
    .await;
    let rows = reminders_of(&server, &token, "routine", timed["id"].as_i64().unwrap()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["offset_value"], 10);
    assert_eq!(rows[0]["offset_unit"], "minutes");
    let (_, empty) = call(
        &server,
        "POST",
        "/api/v1/routines",
        Some(&token),
        routine(serde_json::json!({ "time_of_day": "18:00", "reminders": [] })),
    )
    .await;
    assert!(
        reminders_of(&server, &token, "routine", empty["id"].as_i64().unwrap())
            .await
            .is_empty()
    );
    let (_, untimed) = call(
        &server,
        "POST",
        "/api/v1/routines",
        Some(&token),
        routine(serde_json::json!({})),
    )
    .await;
    assert!(
        reminders_of(&server, &token, "routine", untimed["id"].as_i64().unwrap())
            .await
            .is_empty()
    );

    let (_, allday) = call(
        &server,
        "POST",
        "/api/v1/todos",
        Some(&token),
        serde_json::json!({ "title": "T", "kind": "deadline", "date": "2030-03-07" }),
    )
    .await;
    let rows = reminders_of(&server, &token, "todo", allday["id"].as_i64().unwrap()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["offset_value"], 1);
    assert_eq!(rows[0]["offset_unit"], "days");
    assert_eq!(rows[0]["at_time"], "09:00");
    let (_, hourly) = call(&server, "POST", "/api/v1/todos", Some(&token), serde_json::json!({ "title": "T", "kind": "deadline", "date": "2030-03-07", "time": "15:00" })).await;
    let rows = reminders_of(&server, &token, "todo", hourly["id"].as_i64().unwrap()).await;
    assert_eq!(rows[0]["offset_value"], 60);
    assert_eq!(rows[0]["offset_unit"], "minutes");
    let (_, none) = call(&server, "POST", "/api/v1/todos", Some(&token), serde_json::json!({ "title": "T", "kind": "deadline", "date": "2030-03-07", "reminders": [] })).await;
    assert!(
        reminders_of(&server, &token, "todo", none["id"].as_i64().unwrap())
            .await
            .is_empty()
    );
    let (_, loose) = call(
        &server,
        "POST",
        "/api/v1/todos",
        Some(&token),
        serde_json::json!({ "title": "no deadline" }),
    )
    .await;
    assert!(
        reminders_of(&server, &token, "todo", loose["id"].as_i64().unwrap())
            .await
            .is_empty()
    );

    let prefs = notify::NotificationPrefs {
        enabled: false,
        ..notify::NotificationPrefs::default()
    };
    notify::save_prefs(
        &server.pool,
        user_id(&server, "defaults_user").await,
        &prefs,
    )
    .await
    .unwrap();
    let (_, off) = call(
        &server,
        "POST",
        "/api/v1/routines",
        Some(&token),
        routine(serde_json::json!({ "time_of_day": "18:00" })),
    )
    .await;
    assert!(
        reminders_of(&server, &token, "routine", off["id"].as_i64().unwrap())
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn default_todo_reminder_fires_previous_day_at_nine() {
    let server = setup().await;
    let token = signup(&server, "todo_default").await;
    let (_, todo) = call(
        &server,
        "POST",
        "/api/v1/todos",
        Some(&token),
        serde_json::json!({ "title": "Pay rent", "kind": "deadline", "date": "2030-03-07" }),
    )
    .await;
    assert!(todo["id"].as_i64().is_some());
    let id = user_id(&server, "todo_default").await;
    notify::run_tick(&server.pool, at("2030-03-06T08:50:00Z")).await;
    assert!(notifications(&server.pool, id).await.is_empty());
    notify::run_tick(&server.pool, at("2030-03-06T09:01:00Z")).await;
    let items = notifications(&server.pool, id).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "Pay rent");
    assert_eq!(items[0].body.as_deref(), Some("Deadline tomorrow"));
}

#[tokio::test]
async fn default_routine_reminder_fires_in_tick() {
    let pool = pool().await;
    let user = make_user(&pool, "tick_default", "UTC").await;
    let routine = store::create_routine(
        &pool,
        user,
        &routine_spec("Lift", Some("18:00"), "2030-03-01"),
    )
    .await
    .unwrap();
    store::apply_default_reminders(&pool, user, "routine", routine, true)
        .await
        .unwrap();
    notify::run_tick(&pool, at("2030-03-04T17:51:00Z")).await;
    let items = notifications(&pool, user).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].body.as_deref(), Some("Starts in 10 minutes"));
    let allday = store::create_routine(&pool, user, &routine_spec("Stretch", None, "2030-03-01"))
        .await
        .unwrap();
    store::apply_default_reminders(&pool, user, "routine", allday, false)
        .await
        .unwrap();
    assert!(
        store::list_reminders_for(&pool, user, "routine", &[allday])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn editing_time_drops_reminders_that_become_invalid() {
    let pool = pool().await;
    let user = make_user(&pool, "prune", "UTC").await;
    let today = store::user_today("UTC", 0);
    let routine =
        store::create_routine(&pool, user, &routine_spec("P", Some("10:00"), "2020-01-01"))
            .await
            .unwrap();
    store::replace_reminders(
        &pool,
        user,
        "routine",
        routine,
        &[spec(10, "minutes", None), spec(1, "days", Some("09:00"))],
    )
    .await
    .unwrap();
    let untimed = routine_spec("P", None, "2020-01-01");
    let head = store::update_routine(&pool, user, routine, &untimed, today)
        .await
        .unwrap();
    let rows = store::list_reminders_for(&pool, user, "routine", &[head])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].offset_unit, "days");
    let timed = routine_spec("P", Some("11:00"), "2020-01-01");
    let head = store::update_routine(&pool, user, head, &timed, today)
        .await
        .unwrap();
    let rows = store::list_reminders_for(&pool, user, "routine", &[head])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].at_time.as_deref(), Some("09:00"));
}

#[tokio::test]
async fn backfill_migration_adds_defaults_without_retroactive_firing() {
    let pool = pool().await;
    let user = make_user(&pool, "backfill", "UTC").await;
    let now = Utc::now();
    let soon = (now + chrono::Duration::minutes(5))
        .format("%H:%M")
        .to_string();
    let timed = store::create_routine(
        &pool,
        user,
        &routine_spec("Soon", Some(&soon), &now.date_naive().to_string()),
    )
    .await
    .unwrap();
    let untimed = store::create_routine(
        &pool,
        user,
        &routine_spec("Loose", None, &now.date_naive().to_string()),
    )
    .await
    .unwrap();
    let archived = store::create_routine(
        &pool,
        user,
        &routine_spec("Old", Some("10:00"), &now.date_naive().to_string()),
    )
    .await
    .unwrap();
    store::archive_routine(&pool, user, archived).await.unwrap();
    let kept = store::create_routine(
        &pool,
        user,
        &routine_spec("Kept", Some("10:00"), &now.date_naive().to_string()),
    )
    .await
    .unwrap();
    store::replace_reminders(&pool, user, "routine", kept, &[spec(30, "minutes", None)])
        .await
        .unwrap();
    let deadline_timed = (now + chrono::Duration::minutes(30)).to_rfc3339();
    let todo_timed = store::create_todo(
        &pool,
        user,
        "Soon todo",
        None,
        Some(&deadline_timed),
        "deadline",
        false,
    )
    .await
    .unwrap();
    let todo_allday = store::create_todo(
        &pool,
        user,
        "Tomorrow todo",
        None,
        Some(&(now + chrono::Duration::hours(10)).to_rfc3339()),
        "deadline",
        true,
    )
    .await
    .unwrap();
    let todo_done = store::create_todo(
        &pool,
        user,
        "Done todo",
        None,
        Some(&deadline_timed),
        "deadline",
        false,
    )
    .await
    .unwrap();
    store::toggle_todo(&pool, user, todo_done).await.unwrap();
    store::create_todo(&pool, user, "No deadline", None, None, "todo", false)
        .await
        .unwrap();
    let sql = include_str!("../migrations/0004_notification_defaults.sql");
    let backfill = &sql[sql.find("INSERT").unwrap()..];
    sqlx::raw_sql(backfill).execute(&pool).await.unwrap();
    sqlx::raw_sql(backfill).execute(&pool).await.unwrap();
    let rows = store::list_reminders_for(&pool, user, "routine", &[timed, untimed, archived, kept])
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|r| r.target_id == timed && r.offset_value == 10 && r.offset_unit == "minutes")
    );
    assert!(
        rows.iter()
            .any(|r| r.target_id == kept && r.offset_value == 30)
    );
    let todos =
        store::list_reminders_for(&pool, user, "todo", &[todo_timed, todo_allday, todo_done])
            .await
            .unwrap();
    assert_eq!(todos.len(), 2);
    assert!(
        todos.iter().any(|r| r.target_id == todo_timed
            && r.offset_value == 60
            && r.offset_unit == "minutes")
    );
    assert!(todos.iter().any(|r| r.target_id == todo_allday
        && r.offset_unit == "days"
        && r.at_time.as_deref() == Some("09:00")));
    let (created,): (String,) = sqlx::query_as(
        "SELECT created_at FROM reminders WHERE target_id = $1 AND target_type = 'routine'",
    )
    .bind(timed)
    .fetch_one(&pool)
    .await
    .unwrap();
    let created = DateTime::parse_from_rfc3339(&created)
        .unwrap()
        .with_timezone(&Utc);
    assert!(created > now - chrono::Duration::seconds(5) && created <= Utc::now());
    notify::save_prefs(
        &pool,
        user,
        &notify::NotificationPrefs {
            day_end_enabled: false,
            ..notify::NotificationPrefs::default()
        },
    )
    .await
    .unwrap();
    notify::run_tick(&pool, Utc::now()).await;
    assert!(notifications(&pool, user).await.is_empty());
}

async fn form_call(server: &Server, path: &str, token: &str, body: &str) -> u16 {
    let mut stream = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\nCookie: session={token}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    return text.split_whitespace().nth(1).unwrap().parse().unwrap();
}

#[tokio::test]
async fn ssr_create_applies_defaults_without_reminder_fields() {
    let server = setup().await;
    let token = signup(&server, "ssr_user").await;
    let uid = user_id(&server, "ssr_user").await;
    let status = form_call(
        &server,
        "/routines",
        &token,
        "title=A&kind=weekdays&monday=on&time_of_day=18%3A00",
    )
    .await;
    assert!(status == 303 || status == 302 || status == 200, "{status}");
    let status = form_call(
        &server,
        "/routines",
        &token,
        "title=B&kind=weekdays&monday=on&time_of_day=18%3A00&reminders_json=%5B%5D",
    )
    .await;
    assert!(status == 303 || status == 302 || status == 200, "{status}");
    let status = form_call(&server, "/routines", &token, "title=C&kind=weekdays&monday=on&time_of_day=18%3A00&reminders_json=%5B%7B%22offset_value%22%3A5%2C%22offset_unit%22%3A%22minutes%22%7D%5D").await;
    assert!(status == 303 || status == 302 || status == 200, "{status}");
    let rows: Vec<(String, Option<i64>)> = sqlx::query_as("SELECT r.title, m.offset_value FROM routines r LEFT JOIN reminders m ON m.target_type = 'routine' AND m.target_id = r.id WHERE r.user_id = $1 ORDER BY r.id")
        .bind(uid)
        .fetch_all(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("A".to_string(), Some(10)),
            ("B".to_string(), None),
            ("C".to_string(), Some(5))
        ]
    );
    form_call(
        &server,
        "/todos?date=2030-03-07",
        &token,
        "title=T&kind=deadline&date=2030-03-07",
    )
    .await;
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM reminders WHERE user_id = $1 AND target_type = 'todo' AND offset_unit = 'days'")
        .bind(uid)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

fn encode(raw: &str) -> String {
    let mut out = String::new();
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    return out;
}

async fn raw_call(
    server: &Server,
    method: &str,
    path: &str,
    token: &str,
    form: Option<&str>,
) -> (u16, String, String) {
    let mut stream = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    let body = form.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\nAccept: text/html\r\nCookie: session={token}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .map(|(h, b)| (h.to_string(), b.to_string()))
        .unwrap_or_default();
    return (status, head, body);
}

async fn insert_notification(pool: &SqlitePool, user: i64, title: &str, url: Option<&str>) -> i64 {
    return sqlx::query_scalar(
        "INSERT INTO notifications (user_id, kind, title, body, url, dedupe_key, created_at) VALUES ($1, 'routine_reminder', $2, 'Starts in 10 minutes', $3, $4, $5) RETURNING id",
    )
    .bind(user)
    .bind(title)
    .bind(url)
    .bind(format!("test:{title}"))
    .bind(Utc::now().to_rfc3339())
    .fetch_one(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn notifications_page_empty_state_and_badge() {
    let server = setup().await;
    let token = signup(&server, "ui_empty").await;
    let (status, _, body) = raw_call(&server, "GET", "/notifications", &token, None).await;
    assert_eq!(status, 200);
    assert!(body.contains("No notifications yet"));
    let (status, _, badge) = raw_call(&server, "GET", "/notifications/badge", &token, None).await;
    assert_eq!(status, 200);
    assert!(badge.contains("bell-badge"));
    assert!(badge.contains(" hidden"));
    let uid = user_id(&server, "ui_empty").await;
    insert_notification(&server.pool, uid, "Stretch", Some("/day")).await;
    let (_, _, badge) = raw_call(&server, "GET", "/notifications/badge", &token, None).await;
    assert!(!badge.contains(" hidden"));
    assert!(badge.contains(">1<"));
    let (_, _, body) = raw_call(&server, "GET", "/notifications", &token, None).await;
    assert!(body.contains("Stretch"));
    assert!(body.contains("is-unread"));
    assert!(body.contains("Mark all read"));
}

#[tokio::test]
async fn notification_mark_read_checks_ownership() {
    let server = setup().await;
    let alice = signup(&server, "ui_alice").await;
    let bob = signup(&server, "ui_bob").await;
    let alice_id = user_id(&server, "ui_alice").await;
    let id = insert_notification(
        &server.pool,
        alice_id,
        "Private",
        Some("/day?date=2030-01-01"),
    )
    .await;
    let (status, _, _) = raw_call(
        &server,
        "POST",
        &format!("/notifications/{id}/read"),
        &bob,
        None,
    )
    .await;
    assert_eq!(status, 404);
    let (status, _, _) = raw_call(
        &server,
        "GET",
        &format!("/notifications/{id}/open"),
        &bob,
        None,
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(
        notify::unread_count(&server.pool, alice_id).await.unwrap(),
        1
    );
    let (status, head, _) = raw_call(
        &server,
        "GET",
        &format!("/notifications/{id}/open"),
        &alice,
        None,
    )
    .await;
    assert_eq!(status, 303);
    assert!(
        head.to_lowercase()
            .contains("location: /day?date=2030-01-01")
    );
    assert_eq!(
        notify::unread_count(&server.pool, alice_id).await.unwrap(),
        0
    );
    let second = insert_notification(&server.pool, alice_id, "Second", None).await;
    let third = insert_notification(&server.pool, alice_id, "Third", None).await;
    let (status, _, _) = raw_call(
        &server,
        "POST",
        &format!("/notifications/{second}/read"),
        &alice,
        None,
    )
    .await;
    assert_eq!(status, 303);
    assert_eq!(
        notify::unread_count(&server.pool, alice_id).await.unwrap(),
        1
    );
    let (status, _, _) = raw_call(&server, "POST", "/notifications/read", &alice, None).await;
    assert_eq!(status, 303);
    assert_eq!(
        notify::unread_count(&server.pool, alice_id).await.unwrap(),
        0
    );
    assert!(third > second);
}

#[tokio::test]
async fn settings_notification_prefs_round_trip() {
    let server = setup().await;
    let token = signup(&server, "ui_prefs").await;
    let uid = user_id(&server, "ui_prefs").await;
    let (_, _, page) = raw_call(&server, "GET", "/settings", &token, None).await;
    for field in [
        "name=\"enabled\"",
        "name=\"day_end_time\"",
        "name=\"penalty_enabled\"",
        "name=\"routine_timed_value\"",
        "name=\"routine_allday_time\"",
        "name=\"todo_timed_unit\"",
        "name=\"todo_allday_days\"",
        "name=\"quiet_start\"",
    ] {
        assert!(page.contains(field), "{field}");
    }
    let form = "enabled=on&day_end_enabled=on&day_end_time=21%3A30&routine_timed_on=on&routine_timed_value=2&routine_timed_unit=hours&routine_allday_on=on&routine_allday_time=08%3A15&todo_timed_on=on&todo_timed_value=1&todo_timed_unit=days&todo_allday_on=on&todo_allday_days=3&todo_allday_time=07%3A00&quiet_start=23%3A00&quiet_end=07%3A00";
    let (status, _, _) = raw_call(
        &server,
        "POST",
        "/settings/notifications",
        &token,
        Some(form),
    )
    .await;
    assert_eq!(status, 303);
    let prefs = notify::get_prefs(&server.pool, uid).await.unwrap();
    assert!(prefs.enabled);
    assert!(prefs.day_end_enabled);
    assert!(!prefs.penalty_enabled);
    assert_eq!(prefs.day_end_time, "21:30");
    assert_eq!(prefs.default_routine_timed_minutes, Some(120));
    assert!(prefs.default_routine_allday_enabled);
    assert_eq!(prefs.default_routine_allday_time, "08:15");
    assert_eq!(prefs.default_todo_timed_minutes, Some(1440));
    assert_eq!(prefs.default_todo_allday_days, Some(3));
    assert_eq!(prefs.quiet_start.as_deref(), Some("23:00"));
    let (_, _, page) = raw_call(&server, "GET", "/settings", &token, None).await;
    assert!(page.contains("value=\"21:30\""));
    assert!(page.contains("name=\"routine_timed_value\" min=\"0\" step=\"1\" value=\"2\""));
    assert!(page.contains("<option value=\"hours\" selected>"));
    assert!(page.contains("<option value=\"days\" selected>"));
    let bad = "enabled=on&day_end_time=25%3A99&quiet_start=&quiet_end=";
    let (status, _, page) = raw_call(
        &server,
        "POST",
        "/settings/notifications",
        &token,
        Some(bad),
    )
    .await;
    assert_eq!(status, 422);
    assert!(page.contains("day end time must be HH:MM"));
    let unchanged = notify::get_prefs(&server.pool, uid).await.unwrap();
    assert_eq!(unchanged.day_end_time, "21:30");
    let off = "day_end_time=20%3A00&routine_timed_value=10&todo_timed_value=60&todo_allday_days=1&routine_allday_time=09%3A00&todo_allday_time=09%3A00";
    let (status, _, _) = raw_call(
        &server,
        "POST",
        "/settings/notifications",
        &token,
        Some(off),
    )
    .await;
    assert_eq!(status, 303);
    let prefs = notify::get_prefs(&server.pool, uid).await.unwrap();
    assert!(!prefs.enabled);
    assert_eq!(prefs.default_routine_timed_minutes, None);
    assert_eq!(prefs.default_todo_allday_days, None);
    assert_eq!(prefs.quiet_start, None);
}

#[tokio::test]
async fn routine_form_reminders_round_trip_through_edit_page() {
    let server = setup().await;
    let token = signup(&server, "ui_rem").await;
    let uid = user_id(&server, "ui_rem").await;
    let (_, _, new_page) = raw_call(&server, "GET", "/routines/new", &token, None).await;
    assert!(new_page.contains("data-reminders"));
    assert!(new_page.contains("name=\"reminders_json\""));
    let (status, _, _) = raw_call(
        &server,
        "POST",
        "/routines",
        &token,
        Some("title=Default&kind=weekdays&monday=on&time_of_day=18%3A00"),
    )
    .await;
    assert_eq!(status, 303);
    let json = r#"[{"offset_value":3,"offset_unit":"hours","at_time":null},{"offset_value":1,"offset_unit":"days","at_time":"08:30"}]"#;
    let form = format!(
        "title=Custom&kind=weekdays&monday=on&time_of_day=18%3A00&reminders_json={}",
        encode(json)
    );
    let (status, _, _) = raw_call(&server, "POST", "/routines", &token, Some(&form)).await;
    assert_eq!(status, 303);
    let ids: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, title FROM routines WHERE user_id = $1 ORDER BY id")
            .bind(uid)
            .fetch_all(&server.pool)
            .await
            .unwrap();
    let default_id = ids[0].0;
    let custom_id = ids[1].0;
    let (status, _, page) = raw_call(
        &server,
        "GET",
        &format!("/routines/{default_id}/edit"),
        &token,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(page.contains("reminder-row"));
    assert!(page.contains(
        "class=\"reminder-value\" min=\"0\" max=\"40320\" step=\"1\" required value=\"10\""
    ));
    let (_, _, page) = raw_call(
        &server,
        "GET",
        &format!("/routines/{custom_id}/edit"),
        &token,
        None,
    )
    .await;
    assert!(page.contains("required value=\"3\""));
    assert!(page.contains("<option value=\"hours\" selected>"));
    assert!(page.contains("<option value=\"days\" selected>"));
    assert!(page.contains("value=\"08:30\""));
    let update = format!(
        "title=Custom&kind=weekdays&monday=on&time_of_day=18%3A00&reminders_json={}",
        encode("[]")
    );
    let (status, _, _) = raw_call(
        &server,
        "POST",
        &format!("/routines/{custom_id}"),
        &token,
        Some(&update),
    )
    .await;
    assert_eq!(status, 303);
    let (_, _, page) = raw_call(
        &server,
        "GET",
        &format!("/routines/{custom_id}/edit"),
        &token,
        None,
    )
    .await;
    assert_eq!(page.matches("<div class=\"reminder-row\"").count(), 1);
    let bad = r#"[{"offset_value":1,"offset_unit":"days","at_time":null}]"#;
    let update = format!(
        "title=Custom&kind=weekdays&monday=on&time_of_day=18%3A00&reminders_json={}",
        encode(bad)
    );
    let (status, _, page) = raw_call(
        &server,
        "POST",
        &format!("/routines/{custom_id}"),
        &token,
        Some(&update),
    )
    .await;
    assert_eq!(status, 200);
    assert!(page.contains("reminder time is required"));
    assert!(page.contains("required value=\"1\""));
    let good = r#"[{"offset_value":15,"offset_unit":"minutes","at_time":null}]"#;
    let update = format!(
        "title=Custom&kind=weekdays&monday=on&time_of_day=18%3A00&reminders_json={}",
        encode(good)
    );
    let (status, _, _) = raw_call(
        &server,
        "POST",
        &format!("/routines/{custom_id}"),
        &token,
        Some(&update),
    )
    .await;
    assert_eq!(status, 303);
    let (_, _, page) = raw_call(
        &server,
        "GET",
        &format!("/routines/{custom_id}/edit"),
        &token,
        None,
    )
    .await;
    assert!(page.contains("required value=\"15\""));
}
