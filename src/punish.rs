use chrono::NaiveDate;
use chrono::Utc;
use sqlx::SqliteConnection;
use sqlx::SqlitePool;

use crate::auth::session::SessionUser;
use crate::recurrence::ScheduleKind;
use crate::recurrence::periods_in_range;
use crate::store;

pub const AUTO_CAP_PER_DAY: u32 = 3;

pub const SEED_TITLES: [&str; 8] = [
    "20 pushups",
    "50 jumping jacks",
    "15 min walk",
    "wash the dishes",
    "clean one room",
    "30 min reading",
    "no sugar tomorrow",
    "10 squats",
];

#[derive(Debug, Clone, Copy, Default)]
pub struct EvalStats {
    pub weekday_misses: u32,
    pub count_misses: u32,
    pub draws: u32,
}

#[derive(Debug, Clone)]
pub struct CatalogItem {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub active: bool,
    pub created_by: i64,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct PunishmentItem {
    pub id: i64,
    pub user_id: i64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub source: String,
    pub status: String,
    pub assigned_at: String,
    pub cleared_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PunishmentDetail {
    pub id: i64,
    pub user_id: i64,
    pub username: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub source: String,
    pub status: String,
    pub assigned_at: String,
    pub cleared_at: Option<String>,
    pub missed_routine: Option<String>,
    pub missed_date: Option<String>,
    pub missed_period: Option<String>,
    pub period_start: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PunishOutcome {
    Done,
    NotPending,
    Missing,
}

#[derive(Debug, Clone)]
pub enum AssignTarget {
    Catalog(i64),
    Custom(String),
}

type CatalogRecord = (i64, String, Option<String>, i64, i64, String);

type PunishRecord = (
    i64,
    i64,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
);

type PunishDetailRecord = (
    i64,
    i64,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn catalog_from_row(row: CatalogRecord) -> CatalogItem {
    return CatalogItem {
        id: row.0,
        title: row.1,
        description: row.2,
        active: row.3 != 0,
        created_by: row.4,
        created_at: row.5,
    };
}

fn punishment_from_row(row: PunishRecord) -> PunishmentItem {
    return PunishmentItem {
        id: row.0,
        user_id: row.1,
        title: row.2,
        description: row.3,
        source: row.4,
        status: row.5,
        assigned_at: row.6,
        cleared_at: row.7,
    };
}

fn detail_from_row(row: PunishDetailRecord) -> PunishmentDetail {
    return PunishmentDetail {
        id: row.0,
        user_id: row.1,
        username: row.2,
        title: row.3,
        description: row.4,
        source: row.5,
        status: row.6,
        assigned_at: row.7,
        cleared_at: row.8,
        missed_routine: row.9,
        missed_date: row.10,
        missed_period: row.11,
        period_start: row.12,
    };
}

pub fn valid_title(raw: &str) -> bool {
    let len = raw.trim().chars().count();
    return (1..=200).contains(&len);
}

pub async fn seed_catalog_if_empty(pool: &SqlitePool, user_id: i64) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM punishment_catalog")
        .fetch_one(&mut *tx)
        .await?;
    if count > 0 {
        tx.commit().await?;
        return Ok(());
    }
    let now = Utc::now().to_rfc3339();
    for title in SEED_TITLES {
        sqlx::query(
            "INSERT INTO punishment_catalog (title, description, active, created_by, created_at) VALUES ($1, NULL, 1, $2, $3)",
        )
        .bind(title)
        .bind(user_id)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    return Ok(());
}

pub async fn list_catalog(pool: &SqlitePool) -> Result<Vec<CatalogItem>, sqlx::Error> {
    let rows: Vec<CatalogRecord> = sqlx::query_as(
        "SELECT id, title, description, active, created_by, created_at FROM punishment_catalog ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    return Ok(rows.into_iter().map(catalog_from_row).collect());
}

pub async fn add_catalog_item(
    pool: &SqlitePool,
    created_by: i64,
    title: &str,
    description: Option<&str>,
) -> Result<i64, sqlx::Error> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO punishment_catalog (title, description, active, created_by, created_at) VALUES ($1, $2, 1, $3, $4) RETURNING id",
    )
    .bind(title.trim())
    .bind(description.map(str::trim).filter(|d| !d.is_empty()))
    .bind(created_by)
    .bind(Utc::now().to_rfc3339())
    .fetch_one(pool)
    .await?;
    return Ok(id);
}

pub async fn set_catalog_active(
    pool: &SqlitePool,
    id: i64,
    active: bool,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE punishment_catalog SET active = $1 WHERE id = $2")
        .bind(i64::from(active))
        .bind(id)
        .execute(pool)
        .await?;
    return Ok(result.rows_affected() > 0);
}

pub async fn catalog_active(pool: &SqlitePool, id: i64) -> Result<Option<bool>, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT active FROM punishment_catalog WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    return Ok(row.map(|(active,)| active != 0));
}

pub async fn user_exists(pool: &SqlitePool, id: i64) -> Result<bool, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT id FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    return Ok(row.is_some());
}

pub async fn manual_assign(
    pool: &SqlitePool,
    target_user: i64,
    target: AssignTarget,
) -> Result<i64, sqlx::Error> {
    let id: i64 = match target {
        AssignTarget::Catalog(catalog_id) => {
            sqlx::query_scalar(
                "INSERT INTO punishments (user_id, catalog_id, custom_title, source, status, occurrence_id, assigned_at, cleared_at) \
                 VALUES ($1, $2, NULL, 'manual', 'pending', NULL, $3, NULL) RETURNING id",
            )
            .bind(target_user)
            .bind(catalog_id)
            .bind(Utc::now().to_rfc3339())
            .fetch_one(pool)
            .await?
        }
        AssignTarget::Custom(title) => {
            sqlx::query_scalar(
                "INSERT INTO punishments (user_id, catalog_id, custom_title, source, status, occurrence_id, assigned_at, cleared_at) \
                 VALUES ($1, NULL, $2, 'manual', 'pending', NULL, $3, NULL) RETURNING id",
            )
            .bind(target_user)
            .bind(title.trim())
            .bind(Utc::now().to_rfc3339())
            .fetch_one(pool)
            .await?
        }
    };
    return Ok(id);
}

pub async fn close_punishment(
    pool: &SqlitePool,
    user_id: i64,
    punishment_id: i64,
    waive: bool,
) -> Result<PunishOutcome, sqlx::Error> {
    let status = if waive { "waived" } else { "cleared" };
    let result = sqlx::query(
        "UPDATE punishments SET status = $1, cleared_at = $2 WHERE id = $3 AND user_id = $4 AND status = 'pending'",
    )
    .bind(status)
    .bind(Utc::now().to_rfc3339())
    .bind(punishment_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    if result.rows_affected() > 0 {
        return Ok(PunishOutcome::Done);
    }
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM punishments WHERE id = $1 AND user_id = $2")
            .bind(punishment_id)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    if row.is_some() {
        return Ok(PunishOutcome::NotPending);
    }
    return Ok(PunishOutcome::Missing);
}

pub async fn pending_count(pool: &SqlitePool, user_id: i64) -> Result<u64, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM punishments WHERE user_id = $1 AND status = 'pending'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    return Ok(u64::try_from(count).unwrap_or(0));
}

pub async fn list_punishments(
    pool: &SqlitePool,
    user_id: i64,
    status: &str,
) -> Result<Vec<PunishmentItem>, sqlx::Error> {
    let rows: Vec<PunishRecord> = match status {
        "pending" => {
            sqlx::query_as(
                "SELECT p.id, p.user_id, COALESCE(c.title, p.custom_title), c.description, p.source, p.status, p.assigned_at, p.cleared_at \
                 FROM punishments p LEFT JOIN punishment_catalog c ON c.id = p.catalog_id \
                 WHERE p.user_id = $1 AND p.status = 'pending' \
                 ORDER BY p.assigned_at ASC, p.id ASC",
            )
            .bind(user_id)
            .fetch_all(pool)
            .await?
        }
        "cleared" | "waived" => {
            sqlx::query_as(
                "SELECT p.id, p.user_id, COALESCE(c.title, p.custom_title), c.description, p.source, p.status, p.assigned_at, p.cleared_at \
                 FROM punishments p LEFT JOIN punishment_catalog c ON c.id = p.catalog_id \
                 WHERE p.user_id = $1 AND p.status = $2 \
                 ORDER BY COALESCE(p.cleared_at, p.assigned_at) DESC, p.id DESC",
            )
            .bind(user_id)
            .bind(status)
            .fetch_all(pool)
            .await?
        }
        "history" => {
            sqlx::query_as(
                "SELECT p.id, p.user_id, COALESCE(c.title, p.custom_title), c.description, p.source, p.status, p.assigned_at, p.cleared_at \
                 FROM punishments p LEFT JOIN punishment_catalog c ON c.id = p.catalog_id \
                 WHERE p.user_id = $1 AND p.status IN ('cleared', 'waived') \
                 ORDER BY COALESCE(p.cleared_at, p.assigned_at) DESC, p.id DESC",
            )
            .bind(user_id)
            .fetch_all(pool)
            .await?
        }
        _ => {
            sqlx::query_as(
                "SELECT p.id, p.user_id, COALESCE(c.title, p.custom_title), c.description, p.source, p.status, p.assigned_at, p.cleared_at \
                 FROM punishments p LEFT JOIN punishment_catalog c ON c.id = p.catalog_id \
                 WHERE p.user_id = $1 \
                 ORDER BY COALESCE(p.cleared_at, p.assigned_at) DESC, p.id DESC",
            )
            .bind(user_id)
            .fetch_all(pool)
            .await?
        }
    };
    return Ok(rows.into_iter().map(punishment_from_row).collect());
}

pub async fn list_user_details(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<Vec<PunishmentDetail>, sqlx::Error> {
    let rows: Vec<PunishDetailRecord> = sqlx::query_as(
        "SELECT p.id, p.user_id, u.username, COALESCE(c.title, p.custom_title), c.description, p.source, p.status, p.assigned_at, p.cleared_at, COALESCE(r.title, pr.title), o.due_date, pr.period, p.period_start \
         FROM punishments p \
         JOIN users u ON u.id = p.user_id \
         LEFT JOIN punishment_catalog c ON c.id = p.catalog_id \
         LEFT JOIN occurrences o ON o.id = p.occurrence_id \
         LEFT JOIN routines r ON r.id = o.routine_id \
         LEFT JOIN routines pr ON pr.id = p.routine_id \
         WHERE p.user_id = $1 \
         ORDER BY p.assigned_at ASC, p.id ASC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    return Ok(rows.into_iter().map(detail_from_row).collect());
}

pub async fn list_pending_details(pool: &SqlitePool) -> Result<Vec<PunishmentDetail>, sqlx::Error> {
    let rows: Vec<PunishDetailRecord> = sqlx::query_as(
        "SELECT p.id, p.user_id, u.username, COALESCE(c.title, p.custom_title), c.description, p.source, p.status, p.assigned_at, p.cleared_at, COALESCE(r.title, pr.title), o.due_date, pr.period, p.period_start \
         FROM punishments p \
         JOIN users u ON u.id = p.user_id \
         LEFT JOIN punishment_catalog c ON c.id = p.catalog_id \
         LEFT JOIN occurrences o ON o.id = p.occurrence_id \
         LEFT JOIN routines r ON r.id = o.routine_id \
         LEFT JOIN routines pr ON pr.id = p.routine_id \
         WHERE p.status = 'pending' \
         ORDER BY lower(u.username) ASC, p.assigned_at ASC, p.id ASC",
    )
    .fetch_all(pool)
    .await?;
    return Ok(rows.into_iter().map(detail_from_row).collect());
}

pub async fn list_members(pool: &SqlitePool) -> Result<Vec<(i64, String)>, sqlx::Error> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, username FROM users ORDER BY lower(username) ASC")
            .fetch_all(pool)
            .await?;
    return Ok(rows);
}

pub async fn evaluate_user(
    pool: &SqlitePool,
    user: &SessionUser,
    today: NaiveDate,
) -> Result<EvalStats, sqlx::Error> {
    let mut stats = EvalStats::default();
    let mut tx = pool.begin().await?;
    store::materialize(&mut tx, user.id, today).await?;
    let today_str = today.to_string();
    let misses: Vec<(i64, String)> = sqlx::query_as(
        "SELECT o.id, o.due_date FROM occurrences o \
         JOIN routines r ON r.id = o.routine_id \
         LEFT JOIN completions c ON c.occurrence_id = o.id \
         WHERE r.user_id = $1 AND r.archived_at IS NULL AND o.period_start IS NULL AND o.due_date < $2 AND c.id IS NULL \
         ORDER BY o.id",
    )
    .bind(user.id)
    .bind(&today_str)
    .fetch_all(&mut *tx)
    .await?;
    for (occurrence_id, due_date) in misses {
        let used = auto_assigned_count(&mut tx, user.id, &due_date).await?;
        let catalog_id = if used < i64::from(AUTO_CAP_PER_DAY) {
            draw_active_catalog(&mut tx, user.id).await?
        } else {
            None
        };
        let inserted = sqlx::query(
            "INSERT INTO punishments (user_id, catalog_id, custom_title, source, status, occurrence_id, assigned_at, cleared_at, miss_day) \
             VALUES ($1, $2, NULL, 'auto', 'pending', $3, $4, NULL, $5) ON CONFLICT DO NOTHING",
        )
        .bind(user.id)
        .bind(catalog_id)
        .bind(occurrence_id)
        .bind(Utc::now().to_rfc3339())
        .bind(&due_date)
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() > 0 {
            stats.weekday_misses += 1;
            if catalog_id.is_some() {
                stats.draws += 1;
            }
        }
    }
    let routines = store::active_routines(&mut tx, user.id).await?;
    for routine in &routines {
        let Some(schedule) = store::schedule_from_row(routine) else {
            continue;
        };
        if schedule.kind != ScheduleKind::Count {
            continue;
        }
        let Some(scan_end) = today.pred_opt() else {
            continue;
        };
        for period in periods_in_range(&schedule, schedule.start_date, scan_end) {
            if period.period_end >= today {
                continue;
            }
            let (done,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM completions c JOIN occurrences o ON o.id = c.occurrence_id \
                 WHERE o.routine_id = $1 AND o.period_start = $2",
            )
            .bind(routine.id)
            .bind(period.period_start.to_string())
            .fetch_one(&mut *tx)
            .await?;
            if u32::try_from(done).unwrap_or(0) >= period.required {
                continue;
            }
            let marker = sqlx::query(
                "INSERT INTO period_misses (routine_id, period_start, created_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            )
            .bind(routine.id)
            .bind(period.period_start.to_string())
            .bind(Utc::now().to_rfc3339())
            .execute(&mut *tx)
            .await?;
            if marker.rows_affected() == 0 {
                continue;
            }
            stats.count_misses += 1;
            let miss_day = period.period_end.to_string();
            let used = auto_assigned_count(&mut tx, user.id, &miss_day).await?;
            let catalog_id = if used < i64::from(AUTO_CAP_PER_DAY) {
                draw_active_catalog(&mut tx, user.id).await?
            } else {
                None
            };
            sqlx::query(
                "INSERT INTO punishments (user_id, catalog_id, custom_title, source, status, occurrence_id, assigned_at, cleared_at, miss_day, routine_id, period_start) \
                 VALUES ($1, $2, NULL, 'auto', 'pending', NULL, $3, NULL, $4, $5, $6)",
            )
            .bind(user.id)
            .bind(catalog_id)
            .bind(Utc::now().to_rfc3339())
            .bind(&miss_day)
            .bind(routine.id)
            .bind(period.period_start.to_string())
            .execute(&mut *tx)
            .await?;
            if catalog_id.is_some() {
                stats.draws += 1;
            }
        }
    }
    sqlx::query("UPDATE users SET last_evaluated_day = $1 WHERE id = $2")
        .bind(&today_str)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    return Ok(stats);
}

async fn auto_assigned_count(
    conn: &mut SqliteConnection,
    user_id: i64,
    miss_day: &str,
) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM punishments WHERE user_id = $1 AND source = 'auto' AND miss_day = $2 AND catalog_id IS NOT NULL",
    )
    .bind(user_id)
    .bind(miss_day)
    .fetch_one(&mut *conn)
    .await?;
    return Ok(count);
}

async fn draw_active_catalog(
    conn: &mut SqliteConnection,
    user_id: i64,
) -> Result<Option<i64>, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT c.id FROM punishment_catalog c \
         LEFT JOIN (SELECT catalog_id, COUNT(*) AS n FROM punishments \
                    WHERE user_id = $1 AND status = 'pending' AND catalog_id IS NOT NULL \
                    GROUP BY catalog_id) p ON p.catalog_id = c.id \
         WHERE c.active = 1 \
         ORDER BY COALESCE(p.n, 0), RANDOM() LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await?;
    return Ok(row.map(|(id,)| id));
}
