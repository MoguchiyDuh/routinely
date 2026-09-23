use std::collections::HashMap;
use std::collections::HashSet;

use askama::Template;
use axum::Router;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use chrono::DateTime;
use chrono::Datelike;
use chrono::LocalResult;
use chrono::NaiveDate;
use chrono::TimeZone;
use chrono::Utc;
use chrono_tz::Tz;
use serde::Deserialize;
use serde::Serialize;
use sqlx::SqlitePool;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::punish;
use crate::recurrence::PeriodKind;
use crate::recurrence::ScheduleKind;
use crate::recurrence::occurrences_in_range;
use crate::recurrence::period_bounds;
use crate::recurrence::periods_in_range;
use crate::store;

use axum::routing::get;

use super::AvatarVm;
use super::WebError;
use super::WebResult;
use super::day;
use super::html;
use super::parse_date_param;
use super::shell::Shell;

#[derive(Clone, Copy)]
enum Who {
    Me,
    All,
    User(i64),
}

impl Who {
    fn filter_id(self, me: i64) -> Option<i64> {
        return match self {
            Who::Me => Some(me),
            Who::All => None,
            Who::User(id) => Some(id),
        };
    }
}

#[derive(Template)]
#[template(path = "week.html")]
pub struct WeekPage {
    pub shell: Shell,
    week_start: NaiveDate,
    week_end: NaiveDate,
    prev_href: String,
    next_href: String,
    this_week_href: String,
    chips: Vec<ChipVm>,
    days: Vec<WeekDayVm>,
    hours: Vec<String>,
    now_min: i64,
    has_today: bool,
    has_entries: bool,
}

struct ChipVm {
    label: String,
    href: String,
    active: bool,
    avatar: Option<AvatarVm>,
}

struct WeekDayVm {
    date: NaiveDate,
    weekday_label: String,
    day_num: u32,
    is_today: bool,
    anytime: Vec<ItemVm>,
    blocks: Vec<BlockVm>,
    markers: Vec<TodoMarkerVm>,
    agenda: Vec<ItemVm>,
    is_empty: bool,
}

#[derive(Clone)]
struct OwnerVm {
    username: String,
    color: u8,
}

#[derive(Clone)]
struct ItemVm {
    status: &'static str,
    planned: bool,
    title: String,
    time_label: String,
    count_label: Option<String>,
    pill_class: &'static str,
    pill_word: &'static str,
    pill_icon: &'static str,
    has_pill: bool,
    owner: Option<OwnerVm>,
    occurrence_id: Option<i64>,
    has_pin: bool,
    overdue: bool,
    done: bool,
}

struct BlockVm {
    item: ItemVm,
    start_min: i64,
    len_min: i64,
    col: u32,
    cols: u32,
    aria: String,
}

struct TodoMarkerVm {
    title: String,
    time_label: String,
    start_min: i64,
    done: bool,
    overdue: bool,
    owner: Option<OwnerVm>,
}

struct PendingWeekBlock {
    block: BlockVm,
    start: i64,
    end: i64,
}

struct PageInput<'a> {
    me: i64,
    timezone: &'a str,
    who: Who,
    week_start: NaiveDate,
    week_end: NaiveDate,
    days: Vec<DayCell>,
    members: &'a [store::MemberRow],
    today: NaiveDate,
}

#[derive(Template)]
#[template(path = "_pin.html")]
struct PinFragment {
    occurrence_id: i64,
    pin: WeekPin,
}

#[derive(Serialize)]
struct WeekJson {
    week_start: NaiveDate,
    week_end: NaiveDate,
    days: Vec<DayCell>,
}

#[derive(Serialize)]
struct DayCell {
    date: NaiveDate,
    entries: Vec<WeekEntry>,
}

#[derive(Serialize)]
#[serde(tag = "kind")]
enum WeekEntry {
    #[serde(rename = "occurrence")]
    Occurrence(OccurrenceEntry),
    #[serde(rename = "period")]
    Period(PeriodEntry),
    #[serde(rename = "todo")]
    Todo(TodoEntry),
}

#[derive(Serialize)]
struct OccurrenceEntry {
    occurrence_id: Option<i64>,
    routine_id: i64,
    title: String,
    username: String,
    status: &'static str,
    planned: bool,
    time_of_day: Option<String>,
    time_end: Option<String>,
    time_label: Option<String>,
    pin: Option<WeekPin>,
    #[serde(skip)]
    user_id: i64,
}

#[derive(Serialize)]
struct PeriodEntry {
    routine_id: i64,
    title: String,
    username: String,
    cell_date: NaiveDate,
    period_start: NaiveDate,
    period_end: NaiveDate,
    done: u32,
    required: u32,
    #[serde(skip)]
    user_id: i64,
}

#[derive(Serialize)]
struct TodoEntry {
    todo_id: i64,
    title: String,
    username: String,
    deadline: String,
    local_time: String,
    overdue: bool,
    completed: bool,
    #[serde(skip)]
    user_id: i64,
}

#[derive(Serialize)]
struct WeekPin {
    summary: String,
    links: Vec<String>,
    photos: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WeekQuery {
    date: Option<String>,
    who: Option<String>,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/week", get(week_view))
        .route("/api/v1/week", get(api_week));
}

pub(crate) async fn pin_fragment(
    PageAuth(_auth): PageAuth,
    State(state): State<AppState>,
    Path(occurrence_id): Path<i64>,
) -> WebResult {
    let Some(pin) = store::pin_for_occurrence(&state.pool, occurrence_id).await? else {
        return Err(WebError::NotFound);
    };
    return Ok(html(&PinFragment {
        occurrence_id,
        pin: WeekPin {
            summary: pin.summary,
            links: pin.links,
            photos: pin.photos.iter().map(|path| upload_url(path)).collect(),
        },
    })?);
}

async fn resolve_who(pool: &SqlitePool, raw: Option<&str>, default: Who) -> Result<Who, WebError> {
    let Some(raw) = raw else {
        return Ok(default);
    };
    if raw == "me" {
        return Ok(Who::Me);
    }
    if raw == "all" {
        return Ok(Who::All);
    }
    if let Ok(id) = raw.parse::<i64>() {
        if punish::user_exists(pool, id).await? {
            return Ok(Who::User(id));
        }
        return Err(WebError::NotFound);
    }
    return Ok(default);
}

async fn build_week(
    pool: &SqlitePool,
    timezone: &str,
    anchor: NaiveDate,
    today: NaiveDate,
    who: Option<i64>,
) -> Result<(NaiveDate, NaiveDate, Vec<DayCell>), WebError> {
    let Some((week_start, week_end)) = period_bounds(anchor, PeriodKind::Week) else {
        return Err(WebError::Validation("invalid date"));
    };
    let tz = store::user_timezone(timezone);
    let mut by_date: HashMap<NaiveDate, Vec<WeekEntry>> = HashMap::new();
    let mut occurrences = store::week_occurrences(pool, week_start, week_end).await?;
    if let Some(id) = who {
        occurrences.retain(|occ| occ.user_id == id);
    }
    let mut db_keys: HashSet<(i64, NaiveDate)> = HashSet::new();
    let mut pinned: Vec<i64> = Vec::new();
    for occ in &occurrences {
        db_keys.insert((occ.routine_id, occ.due_date));
        if occ.has_pin {
            pinned.push(occ.occurrence_id);
        }
    }
    let mut pins: HashMap<i64, store::PinRow> = store::pins_for_occurrences(pool, &pinned)
        .await?
        .into_iter()
        .map(|row| (row.occurrence_id, row))
        .collect();
    for occ in occurrences {
        let pin = pins.remove(&occ.occurrence_id).map(|row| WeekPin {
            summary: row.summary,
            links: row.links,
            photos: row.photos.iter().map(|path| upload_url(path)).collect(),
        });
        let status = occurrence_status(occ.completed, occ.due_date, today);
        let time_label = time_label(occ.time_of_day.clone(), occ.time_end.clone());
        by_date
            .entry(occ.due_date)
            .or_default()
            .push(WeekEntry::Occurrence(OccurrenceEntry {
                occurrence_id: Some(occ.occurrence_id),
                routine_id: occ.routine_id,
                title: occ.title,
                username: occ.username,
                status,
                planned: false,
                time_of_day: occ.time_of_day,
                time_end: occ.time_end,
                time_label,
                pin,
                user_id: occ.user_id,
            }));
    }
    let mut period_counts: HashMap<(i64, String), u32> = HashMap::new();
    for (routine_id, period_start, count) in store::period_completion_counts(pool).await? {
        period_counts.insert(
            (routine_id, period_start),
            u32::try_from(count).unwrap_or(0),
        );
    }
    for holder in store::active_routines_with_users(pool).await? {
        if who.is_some_and(|id| holder.routine.user_id != id) {
            continue;
        }
        let Some(schedule) = store::schedule_from_row(&holder.routine) else {
            continue;
        };
        match schedule.kind {
            ScheduleKind::Weekdays => {
                for date in occurrences_in_range(&schedule, week_start, week_end) {
                    if db_keys.contains(&(holder.routine.id, date)) {
                        continue;
                    }
                    let status = occurrence_status(false, date, today);
                    let time_label = time_label(
                        holder.routine.time_of_day.clone(),
                        holder.routine.time_end.clone(),
                    );
                    by_date
                        .entry(date)
                        .or_default()
                        .push(WeekEntry::Occurrence(OccurrenceEntry {
                            occurrence_id: None,
                            routine_id: holder.routine.id,
                            title: holder.routine.title.clone(),
                            username: holder.username.clone(),
                            status,
                            planned: true,
                            time_of_day: holder.routine.time_of_day.clone(),
                            time_end: holder.routine.time_end.clone(),
                            time_label,
                            pin: None,
                            user_id: holder.routine.user_id,
                        }));
                }
            }
            ScheduleKind::Count => {
                for period in periods_in_range(&schedule, week_start, week_end) {
                    let done = period_counts
                        .get(&(holder.routine.id, period.period_start.to_string()))
                        .copied()
                        .unwrap_or(0);
                    if done >= period.required {
                        continue;
                    }
                    let cell_date = period.period_start.max(week_start);
                    by_date
                        .entry(cell_date)
                        .or_default()
                        .push(WeekEntry::Period(PeriodEntry {
                            routine_id: holder.routine.id,
                            title: holder.routine.title.clone(),
                            username: holder.username.clone(),
                            cell_date,
                            period_start: period.period_start,
                            period_end: period.period_end,
                            done,
                            required: period.required,
                            user_id: holder.routine.user_id,
                        }));
                }
            }
        }
    }
    let range_start = local_midnight_utc(tz, week_start).to_rfc3339();
    let range_end = match week_end.succ_opt() {
        Some(next) => local_midnight_utc(tz, next).to_rfc3339(),
        None => local_midnight_utc(tz, week_end).to_rfc3339(),
    };
    let now = Utc::now();
    for todo in store::todos_with_deadline_between(pool, &range_start, &range_end).await? {
        if who.is_some_and(|id| todo.user_id != id) {
            continue;
        }
        let Ok(deadline) = DateTime::parse_from_rfc3339(&todo.deadline) else {
            continue;
        };
        let local = deadline.with_timezone(&tz);
        let day = local.date_naive();
        if day < week_start || day > week_end {
            continue;
        }
        let completed = todo.completed_at.is_some();
        let overdue = !completed && deadline.with_timezone(&Utc) < now;
        by_date
            .entry(day)
            .or_default()
            .push(WeekEntry::Todo(TodoEntry {
                todo_id: todo.id,
                title: todo.title,
                username: todo.username,
                deadline: todo.deadline,
                local_time: local.format("%H:%M").to_string(),
                overdue,
                completed,
                user_id: todo.user_id,
            }));
    }
    let mut days: Vec<DayCell> = Vec::new();
    let mut cursor = week_start;
    while cursor <= week_end {
        let mut entries = by_date.remove(&cursor).unwrap_or_default();
        entries.sort_by(compare_entries);
        days.push(DayCell {
            date: cursor,
            entries,
        });
        cursor = match cursor.succ_opt() {
            Some(next) => next,
            None => break,
        };
    }
    return Ok((week_start, week_end, days));
}

async fn week_view(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Query(query): Query<WeekQuery>,
    headers: axum::http::HeaderMap,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    let anchor = parse_date_param(query.date.as_deref(), today)?;
    let who = resolve_who(&state.pool, query.who.as_deref(), Who::Me).await?;
    let shell = Shell::load(&state, &auth, "week", &headers).await?;
    let (week_start, week_end, days) = build_week(
        &state.pool,
        &auth.timezone,
        anchor,
        today,
        who.filter_id(auth.id),
    )
    .await?;
    let members = store::list_members(&state.pool).await?;
    let page = build_page(
        shell,
        PageInput {
            me: auth.id,
            timezone: &auth.timezone,
            who,
            week_start,
            week_end,
            days,
            members: &members,
            today,
        },
    );
    return Ok(html(&page)?);
}

async fn api_week(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Query(query): Query<WeekQuery>,
) -> Result<axum::Json<WeekJson>, WebError> {
    let today = store::user_today(&auth.timezone);
    let anchor = parse_date_param(query.date.as_deref(), today)?;
    let who = resolve_who(&state.pool, query.who.as_deref(), Who::All).await?;
    let (week_start, week_end, days) = build_week(
        &state.pool,
        &auth.timezone,
        anchor,
        today,
        who.filter_id(auth.id),
    )
    .await?;
    return Ok(axum::Json(WeekJson {
        week_start,
        week_end,
        days,
    }));
}

fn build_page(shell: Shell, input: PageInput<'_>) -> WeekPage {
    let mut colors: HashMap<i64, u8> = HashMap::new();
    let mut avatars: HashMap<i64, AvatarVm> = HashMap::new();
    for member in input.members {
        let avatar = AvatarVm::for_user(
            member.id,
            &member.username,
            member.avatar_path.as_deref(),
            member.avatar_icon.as_deref(),
            member.avatar_color,
        );
        colors.insert(member.id, avatar.color);
        avatars.insert(member.id, avatar);
    }
    let (prev_week, next_week) = neighbor_weeks(input.week_start, input.week_end);
    let suffix = match input.who {
        Who::Me => String::new(),
        Who::All => "?who=all".to_string(),
        Who::User(id) => format!("?who={id}"),
    };
    let chips = build_chips(
        input.me,
        input.who,
        input.week_start,
        &avatars,
        input.members,
    );
    let all_mode = matches!(input.who, Who::All);
    let mut days: Vec<WeekDayVm> = Vec::new();
    for cell in input.days {
        days.push(build_day_vm(cell, input.today, all_mode, &colors));
    }
    let has_today = days.iter().any(|vm| vm.is_today);
    let has_entries = days.iter().any(|vm| !vm.is_empty);
    return WeekPage {
        shell,
        week_start: input.week_start,
        week_end: input.week_end,
        prev_href: format!("/week?date={prev_week}{suffix}"),
        next_href: format!("/week?date={next_week}{suffix}"),
        this_week_href: format!("/week{suffix}"),
        chips,
        days,
        hours: day::hour_labels(),
        now_min: day::local_now_minutes(input.timezone),
        has_today,
        has_entries,
    };
}

fn build_chips(
    me: i64,
    who: Who,
    week_start: NaiveDate,
    avatars: &HashMap<i64, AvatarVm>,
    members: &[store::MemberRow],
) -> Vec<ChipVm> {
    let mut chips = Vec::new();
    chips.push(ChipVm {
        label: "Me".to_string(),
        href: "/week".to_string(),
        active: matches!(who, Who::Me),
        avatar: avatars.get(&me).cloned(),
    });
    for member in members {
        if member.id == me {
            continue;
        }
        chips.push(ChipVm {
            label: member.username.clone(),
            href: format!("/week?date={week_start}&who={}", member.id),
            active: matches!(who, Who::User(id) if id == member.id),
            avatar: avatars.get(&member.id).cloned(),
        });
    }
    chips.push(ChipVm {
        label: "Everyone".to_string(),
        href: format!("/week?date={week_start}&who=all"),
        active: matches!(who, Who::All),
        avatar: None,
    });
    return chips;
}

fn build_day_vm(
    cell: DayCell,
    today: NaiveDate,
    all_mode: bool,
    colors: &HashMap<i64, u8>,
) -> WeekDayVm {
    let mut anytime: Vec<ItemVm> = Vec::new();
    let mut pending: Vec<PendingWeekBlock> = Vec::new();
    let mut markers: Vec<TodoMarkerVm> = Vec::new();
    for entry in cell.entries {
        match entry {
            WeekEntry::Occurrence(e) => {
                let owner = owner_vm(all_mode, e.user_id, &e.username, colors);
                let (pill_class, pill_icon, pill_word) = occurrence_pill(e.status, e.planned);
                let time_label = e.time_label.clone().unwrap_or_default();
                let aria = block_aria(&e.title, &time_label, pill_word, &owner);
                let item = ItemVm {
                    status: e.status,
                    planned: e.planned,
                    title: e.title,
                    time_label,
                    count_label: None,
                    pill_class,
                    pill_word,
                    pill_icon,
                    has_pill: true,
                    owner,
                    occurrence_id: e.occurrence_id,
                    has_pin: e.pin.is_some() && e.occurrence_id.is_some(),
                    overdue: false,
                    done: e.status == "completed",
                };
                let start = day::parse_time_minutes(e.time_of_day.as_deref());
                match start {
                    Some(start_min) => {
                        let len_min = day::block_len(
                            start_min,
                            day::parse_time_minutes(e.time_end.as_deref()),
                        );
                        pending.push(PendingWeekBlock {
                            block: BlockVm {
                                item,
                                start_min,
                                len_min,
                                col: 1,
                                cols: 1,
                                aria,
                            },
                            start: start_min,
                            end: start_min + len_min,
                        });
                    }
                    None => anytime.push(item),
                }
            }
            WeekEntry::Period(p) => {
                anytime.push(ItemVm {
                    status: "open",
                    planned: false,
                    title: p.title,
                    time_label: String::new(),
                    count_label: Some(format!(
                        "{}/{} {}",
                        p.done,
                        p.required,
                        period_span_word(p.period_start, p.period_end)
                    )),
                    pill_class: "",
                    pill_word: "",
                    pill_icon: "",
                    has_pill: false,
                    owner: owner_vm(all_mode, p.user_id, &p.username, colors),
                    occurrence_id: None,
                    has_pin: false,
                    overdue: false,
                    done: false,
                });
            }
            WeekEntry::Todo(t) => {
                let start_min = day::parse_time_minutes(Some(&t.local_time)).unwrap_or(0);
                markers.push(TodoMarkerVm {
                    title: t.title,
                    time_label: t.local_time,
                    start_min,
                    done: t.completed,
                    overdue: t.overdue,
                    owner: owner_vm(all_mode, t.user_id, &t.username, colors),
                });
            }
        }
    }
    pending.sort_by_key(|p| (p.start, p.end));
    let spans: Vec<(i64, i64)> = pending.iter().map(|p| (p.start, p.end)).collect();
    let assigned = day::overlap_columns(&spans);
    let mut blocks: Vec<BlockVm> = Vec::new();
    for (item, (col, cols)) in pending.into_iter().zip(assigned) {
        let mut block = item.block;
        block.col = col;
        block.cols = cols;
        blocks.push(block);
    }
    markers.sort_by_key(|m| m.start_min);
    let mut timed: Vec<(i64, ItemVm)> = blocks
        .iter()
        .map(|b| (b.start_min, b.item.clone()))
        .collect();
    for m in &markers {
        timed.push((m.start_min, marker_item(m)));
    }
    timed.sort_by_key(|(start, _)| *start);
    let mut agenda: Vec<ItemVm> = anytime.clone();
    agenda.extend(timed.into_iter().map(|(_, item)| item));
    let is_empty = anytime.is_empty() && blocks.is_empty() && markers.is_empty();
    return WeekDayVm {
        date: cell.date,
        weekday_label: cell.date.format("%a").to_string(),
        day_num: cell.date.day(),
        is_today: cell.date == today,
        anytime,
        blocks,
        markers,
        agenda,
        is_empty,
    };
}

fn owner_vm(
    all_mode: bool,
    user_id: i64,
    username: &str,
    colors: &HashMap<i64, u8>,
) -> Option<OwnerVm> {
    if !all_mode {
        return None;
    }
    let color = colors.get(&user_id).copied().unwrap_or(0);
    return Some(OwnerVm {
        username: username.to_string(),
        color,
    });
}

fn occurrence_pill(
    status: &'static str,
    planned: bool,
) -> (&'static str, &'static str, &'static str) {
    if planned {
        return ("pill-planned", "circle-dashed", "planned");
    }
    return day::status_meta(status);
}

fn block_aria(title: &str, time_label: &str, status_word: &str, owner: &Option<OwnerVm>) -> String {
    let mut aria = format!("{title}, {time_label}, {status_word}");
    if let Some(o) = owner {
        aria.push_str(", ");
        aria.push_str(&o.username);
    }
    return aria;
}

fn marker_item(marker: &TodoMarkerVm) -> ItemVm {
    let (pill_class, pill_word, pill_icon) = if marker.done {
        ("pill-done", "done", "circle-check")
    } else if marker.overdue {
        ("pill-missed", "overdue", "circle-alert")
    } else {
        ("pill-open", "open", "circle-dashed")
    };
    return ItemVm {
        status: "open",
        planned: false,
        title: marker.title.clone(),
        time_label: marker.time_label.clone(),
        count_label: None,
        pill_class,
        pill_word,
        pill_icon,
        has_pill: true,
        owner: marker.owner.clone(),
        occurrence_id: None,
        has_pin: false,
        overdue: marker.overdue,
        done: marker.done,
    };
}

fn neighbor_weeks(week_start: NaiveDate, week_end: NaiveDate) -> (NaiveDate, NaiveDate) {
    let prev = week_start
        .pred_opt()
        .and_then(|date| period_bounds(date, PeriodKind::Week))
        .map(|(start, _)| start)
        .unwrap_or(week_start);
    let next = week_end
        .succ_opt()
        .and_then(|date| period_bounds(date, PeriodKind::Week))
        .map(|(start, _)| start)
        .unwrap_or(week_start);
    return (prev, next);
}

fn occurrence_status(completed: bool, date: NaiveDate, today: NaiveDate) -> &'static str {
    if completed {
        return "completed";
    }
    if date < today {
        return "missed";
    }
    return "open";
}

fn time_label(start: Option<String>, end: Option<String>) -> Option<String> {
    let start = start?;
    return match end {
        Some(end) => Some(format!("{start}\u{2013}{end}")),
        None => Some(start),
    };
}

fn upload_url(file_path: &str) -> String {
    return format!("/uploads/{file_path}");
}

fn local_midnight_utc(tz: Tz, date: NaiveDate) -> DateTime<Utc> {
    let naive = match date.and_hms_opt(0, 0, 0) {
        Some(value) => value,
        None => return Utc::now(),
    };
    let local = match tz.from_local_datetime(&naive) {
        LocalResult::Single(value) => value,
        LocalResult::Ambiguous(value, _) => value,
        LocalResult::None => return Utc.from_utc_datetime(&naive),
    };
    return local.with_timezone(&Utc);
}

fn entry_rank(entry: &WeekEntry) -> u8 {
    return match entry {
        WeekEntry::Occurrence(_) => 0,
        WeekEntry::Period(_) => 1,
        WeekEntry::Todo(_) => 2,
    };
}

fn compare_entries(a: &WeekEntry, b: &WeekEntry) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let rank = entry_rank(a).cmp(&entry_rank(b));
    if rank != Ordering::Equal {
        return rank;
    }
    return match (a, b) {
        (WeekEntry::Occurrence(x), WeekEntry::Occurrence(y)) => {
            let key_x = (
                x.time_of_day.is_none(),
                x.time_of_day.as_deref().unwrap_or(""),
                x.title.as_str(),
            );
            let key_y = (
                y.time_of_day.is_none(),
                y.time_of_day.as_deref().unwrap_or(""),
                y.title.as_str(),
            );
            key_x.cmp(&key_y)
        }
        (WeekEntry::Period(x), WeekEntry::Period(y)) => {
            (x.cell_date, x.routine_id).cmp(&(y.cell_date, y.routine_id))
        }
        (WeekEntry::Todo(x), WeekEntry::Todo(y)) => x.deadline.cmp(&y.deadline),
        _ => Ordering::Equal,
    };
}

fn period_span_word(start: NaiveDate, end: NaiveDate) -> &'static str {
    return match (end - start).num_days() {
        0 => "today",
        6 => "this week",
        span if span >= 364 => "this year",
        _ => "this month",
    };
}
