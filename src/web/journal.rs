use std::collections::BTreeMap;
use std::collections::HashMap;

use askama::Template;
use axum::Router;
use axum::extract::Query;
use axum::extract::State;
use axum::routing::get;
use chrono::Datelike;
use chrono::Days;
use chrono::NaiveDate;
use serde::Deserialize;

use crate::app::AppState;
use crate::auth::extract::PageAuth;
use crate::store;

use super::WebError;
use super::WebResult;
use super::html;
use super::shell::Shell;

const MAX_QUERY_CHARS: usize = 100;
const RANGES: [(&str, &str); 5] = [
    ("week", "This week"),
    ("7d", "Last 7 days"),
    ("30d", "Last 30 days"),
    ("all", "All time"),
    ("custom", "Custom"),
];

#[derive(Debug, Deserialize)]
struct JournalQuery {
    range: Option<String>,
    from: Option<String>,
    to: Option<String>,
    q: Option<String>,
    who: Option<String>,
    routine: Option<String>,
}

struct RoutineOption {
    title: String,
    selected: bool,
}

#[derive(Template)]
#[template(path = "journal.html")]
struct JournalPage {
    shell: Shell,
    ranges: Vec<RangeChip>,
    range: &'static str,
    from_value: String,
    to_value: String,
    q: String,
    days: Vec<JournalDayVm>,
    note_count: usize,
    proof_count: usize,
    scope_label: String,
    picker: super::week::PickerVm,
    readonly: bool,
    who_q: String,
    who_value: String,
    routine_options: Vec<RoutineOption>,
    routine_value: String,
    keep_q: String,
}

struct RangeChip {
    value: &'static str,
    label: &'static str,
    active: bool,
}

struct JournalDayVm {
    date: NaiveDate,
    label: String,
    note: Option<String>,
    proofs: Vec<JournalProofVm>,
}

struct JournalProofVm {
    routine_id: i64,
    title: String,
    summary: String,
    notes: Option<String>,
    links: Vec<String>,
    photos: Vec<String>,
}

pub fn router() -> Router<AppState> {
    return Router::new().route("/journal", get(journal_view));
}

fn parse_optional_date(raw: Option<&str>) -> Option<NaiveDate> {
    return raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok());
}

fn like_pattern(raw: &str) -> Option<String> {
    let trimmed: String = raw.trim().chars().take(MAX_QUERY_CHARS).collect();
    if trimmed.is_empty() {
        return None;
    }
    let mut escaped = String::with_capacity(trimmed.len() + 2);
    escaped.push('%');
    for ch in trimmed.chars() {
        if ch == '%' || ch == '_' || ch == '\\' {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped.push('%');
    return Some(escaped);
}

fn resolve_range(
    raw: &str,
    today: NaiveDate,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> (&'static str, Option<NaiveDate>, Option<NaiveDate>) {
    return match raw {
        "7d" => ("7d", today.checked_sub_days(Days::new(6)), Some(today)),
        "30d" => ("30d", today.checked_sub_days(Days::new(29)), Some(today)),
        "all" => ("all", None, None),
        "custom" => match (from, to) {
            (Some(a), Some(b)) if a > b => ("custom", Some(b), Some(a)),
            _ => ("custom", from, to),
        },
        _ => {
            let monday = today
                .checked_sub_days(Days::new(u64::from(
                    today.weekday().num_days_from_monday(),
                )))
                .unwrap_or(today);
            ("week", Some(monday), monday.checked_add_days(Days::new(6)))
        }
    };
}

async fn journal_view(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Query(query): Query<JournalQuery>,
    headers: axum::http::HeaderMap,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    let (range, from, to) = resolve_range(
        query.range.as_deref().unwrap_or("week"),
        today,
        parse_optional_date(query.from.as_deref()),
        parse_optional_date(query.to.as_deref()),
    );
    let q = query.q.unwrap_or_default();
    let pattern = like_pattern(&q);
    let members = store::list_members(&state.pool).await?;
    let target = match query.who.as_deref().map(str::trim) {
        None | Some("") | Some("me") => auth.id,
        Some(raw) => match raw.parse::<i64>() {
            Ok(id) if members.iter().any(|member| member.id == id) => id,
            _ => return Err(WebError::NotFound),
        },
    };
    let readonly = target != auth.id;
    let shell = Shell::load(&state, &auth, "journal", &headers).await?;
    let routine_value: String = query
        .routine
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .chars()
        .take(MAX_QUERY_CHARS)
        .collect();
    let routine_filter = Some(routine_value.as_str()).filter(|value| !value.is_empty());
    let notes = store::journal_notes(&state.pool, target, from, to, pattern.as_deref()).await?;
    let pins = store::journal_pins(
        &state.pool,
        target,
        from,
        to,
        pattern.as_deref(),
        routine_filter,
    )
    .await?;
    let routine_key = routine_value.to_lowercase();
    let mut routine_options: Vec<RoutineOption> =
        store::journal_routine_titles(&state.pool, target)
            .await?
            .into_iter()
            .map(|title| RoutineOption {
                selected: title.to_lowercase() == routine_key,
                title,
            })
            .collect();
    if routine_filter.is_some() && !routine_options.iter().any(|option| option.selected) {
        routine_options.insert(
            0,
            RoutineOption {
                title: routine_value.clone(),
                selected: true,
            },
        );
    }
    let occurrence_ids: Vec<i64> = pins.iter().map(|pin| pin.1).collect();
    let attachments: HashMap<i64, store::PinRow> =
        store::pins_for_occurrences(&state.pool, &occurrence_ids)
            .await?
            .into_iter()
            .map(|row| (row.occurrence_id, row))
            .collect();
    let mut by_day: BTreeMap<NaiveDate, JournalDayVm> = BTreeMap::new();
    let mut note_count = 0;
    let mut proof_count = 0;
    for (day_raw, body) in notes {
        let Ok(date) = NaiveDate::parse_from_str(&day_raw, "%Y-%m-%d") else {
            continue;
        };
        note_count += 1;
        by_day.entry(date).or_insert_with(|| empty_day(date)).note = Some(body);
    }
    for (day_raw, occurrence_id, routine_id, title, summary, notes) in pins {
        let Ok(date) = NaiveDate::parse_from_str(&day_raw, "%Y-%m-%d") else {
            continue;
        };
        proof_count += 1;
        let (links, photos) = match attachments.get(&occurrence_id) {
            Some(row) => (
                row.links.clone(),
                row.photos
                    .iter()
                    .map(|path| format!("/uploads/{path}"))
                    .collect(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        by_day
            .entry(date)
            .or_insert_with(|| empty_day(date))
            .proofs
            .push(JournalProofVm {
                routine_id,
                title,
                summary,
                notes,
                links,
                photos,
            });
    }
    if routine_filter.is_some() {
        by_day.retain(|_, day| !day.proofs.is_empty());
        note_count = by_day.values().filter(|day| day.note.is_some()).count();
    }
    let days: Vec<JournalDayVm> = by_day.into_values().rev().collect();
    let ranges = RANGES
        .iter()
        .map(|(value, label)| RangeChip {
            value,
            label,
            active: *value == range,
        })
        .collect();
    let scope_label = match (from, to) {
        (Some(a), Some(b)) => format!("{a} \u{2013} {b}"),
        (Some(a), None) => format!("from {a}"),
        (None, Some(b)) => format!("until {b}"),
        (None, None) => "all time".to_string(),
    };
    let from_value: String = from.map(|day| day.to_string()).unwrap_or_default();
    let to_value: String = to.map(|day| day.to_string()).unwrap_or_default();
    let picker = super::week::build_picker_with(
        "/journal",
        auth.id,
        Some(target),
        &members,
        "journal",
        |who| {
            let mut pairs: Vec<(&str, String)> = vec![("range", range.to_string())];
            if range == "custom" {
                pairs.push(("from", from_value.clone()));
                pairs.push(("to", to_value.clone()));
            }
            if !q.is_empty() {
                pairs.push(("q", q.clone()));
            }
            if !routine_value.is_empty() {
                pairs.push(("routine", routine_value.clone()));
            }
            if let Some(id) = who.filter(|id| *id != auth.id) {
                pairs.push(("who", id.to_string()));
            }
            return serde_urlencoded::to_string(&pairs).unwrap_or_default();
        },
    );
    let who_q = if readonly {
        format!("&who={target}")
    } else {
        String::new()
    };
    let keep_q = if routine_value.is_empty() {
        who_q.clone()
    } else {
        format!(
            "{who_q}&{}",
            serde_urlencoded::to_string([("routine", routine_value.as_str())]).unwrap_or_default()
        )
    };
    let page = JournalPage {
        shell,
        ranges,
        range,
        from_value,
        to_value,
        q,
        days,
        note_count,
        proof_count,
        scope_label,
        picker,
        readonly,
        who_q,
        who_value: target.to_string(),
        routine_options,
        routine_value,
        keep_q,
    };
    return Ok(html(&page)?);
}

fn empty_day(date: NaiveDate) -> JournalDayVm {
    return JournalDayVm {
        date,
        label: date.format("%A, %-d %B %Y").to_string(),
        note: None,
        proofs: Vec::new(),
    };
}
