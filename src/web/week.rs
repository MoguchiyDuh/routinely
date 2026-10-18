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
    picker: PickerVm,
    days: Vec<WeekDayVm>,
    past_days: Vec<WeekDayVm>,
    cols: Vec<GridColVm>,
    hours: Vec<String>,
    grid_hours: i64,
    has_all_day: bool,
    goals: Vec<GoalBarVm>,
    who_param: String,
    who_q: String,
    switch_date: NaiveDate,
    has_entries: bool,
}

struct GridColVm {
    date: NaiveDate,
    weekday_label: String,
    day_num: u32,
    is_today: bool,
    is_past: bool,
    all_day: Vec<ItemVm>,
    blocks: Vec<ItemVm>,
    now_top: Option<String>,
}

const GRID_DEFAULT_START: i64 = 8 * 60;
const GRID_DEFAULT_END: i64 = 20 * 60;
const GRID_CASCADE_MAX: u32 = 3;
const GRID_CASCADE_STEP: f64 = 40.0;

pub(crate) struct ChipVm {
    pub label: String,
    pub href: String,
    pub active: bool,
    pub avatar: Option<AvatarVm>,
}

pub(crate) struct PickerVm {
    pub current: ChipVm,
    pub chips: Vec<ChipVm>,
}

pub(crate) fn build_picker(
    base: &str,
    me: i64,
    selected: Option<i64>,
    date: NaiveDate,
    members: &[store::MemberRow],
) -> PickerVm {
    return build_picker_with(base, me, selected, members, |who| match who {
        Some(id) if id == me => format!("date={date}"),
        Some(id) => format!("date={date}&who={id}"),
        None => format!("date={date}&who=all"),
    });
}

pub(crate) fn build_picker_with(
    base: &str,
    me: i64,
    selected: Option<i64>,
    members: &[store::MemberRow],
    query_for: impl Fn(Option<i64>) -> String,
) -> PickerVm {
    let avatar_of = |member: &store::MemberRow| {
        return AvatarVm::for_user(
            member.id,
            &member.username,
            member.avatar_path.as_deref(),
            member.avatar_icon.as_deref(),
            member.avatar_color,
        );
    };
    let mut chips = Vec::new();
    if let Some(member) = members.iter().find(|member| member.id == me) {
        chips.push(ChipVm {
            label: "Me".to_string(),
            href: format!("{base}?{}", query_for(Some(me))),
            active: selected == Some(me),
            avatar: Some(avatar_of(member)),
        });
    }
    for member in members {
        if member.id == me {
            continue;
        }
        chips.push(ChipVm {
            label: member.username.clone(),
            href: format!("{base}?{}", query_for(Some(member.id))),
            active: selected == Some(member.id),
            avatar: Some(avatar_of(member)),
        });
    }
    let current = chips
        .iter()
        .find(|chip| chip.active)
        .map(|chip| ChipVm {
            label: chip.label.clone(),
            href: chip.href.clone(),
            active: true,
            avatar: chip.avatar.clone(),
        })
        .unwrap_or(ChipVm {
            label: "Everyone".to_string(),
            href: format!("{base}?{}", query_for(None)),
            active: true,
            avatar: None,
        });
    return PickerVm { current, chips };
}

#[derive(Clone)]
struct WeekDayVm {
    date: NaiveDate,
    weekday_label: String,
    day_num: u32,
    is_today: bool,
    is_past: bool,
    agenda: Vec<ItemVm>,
    all_day: Vec<ItemVm>,
    timed: Vec<ItemVm>,
    is_empty: bool,
}

#[derive(Clone)]
struct OwnerVm {
    username: String,
    color: u8,
}

#[derive(Clone)]
struct ItemVm {
    kind: &'static str,
    href: String,
    routine_id: Option<i64>,
    todo_id: Option<i64>,
    status: &'static str,
    planned: bool,
    title: String,
    time_label: String,
    start_label: String,
    count_label: Option<String>,
    pill_class: &'static str,
    pill_word: &'static str,
    pill_icon: &'static str,
    has_pill: bool,
    owner: Option<OwnerVm>,
    occurrence_id: Option<i64>,
    has_pin: bool,
    done: bool,
    is_goal: bool,
    can_tick: bool,
    start_min: i64,
    end_min: i64,
    grid_style: String,
}

struct PageInput<'a> {
    me: i64,
    who: Who,
    week_start: NaiveDate,
    week_end: NaiveDate,
    days: Vec<DayCell>,
    members: &'a [store::MemberRow],
    today: NaiveDate,
    now_min: i64,
    goals: Vec<GoalBarVm>,
    hidden: HashSet<i64>,
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
    #[serde(skip)]
    is_goal: bool,
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
    todo_kind: String,
    all_day: bool,
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
        .route("/month", get(month_view))
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
    weekly_as_bars: bool,
) -> Result<(NaiveDate, NaiveDate, Vec<DayCell>), WebError> {
    let Some((week_start, week_end)) = period_bounds(anchor, PeriodKind::Week) else {
        return Err(WebError::Validation("invalid date"));
    };
    let days =
        build_cells(pool, timezone, week_start, week_end, today, who, weekly_as_bars).await?;
    return Ok((week_start, week_end, days));
}

async fn build_cells(
    pool: &SqlitePool,
    timezone: &str,
    week_start: NaiveDate,
    week_end: NaiveDate,
    today: NaiveDate,
    who: Option<i64>,
    weekly_as_bars: bool,
) -> Result<Vec<DayCell>, WebError> {
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
                is_goal: occ.is_goal,
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
                            is_goal: false,
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
                if weekly_as_bars && schedule.period == PeriodKind::Week {
                    continue;
                }
                for period in periods_in_range(&schedule, week_start, week_end) {
                    let done = period_counts
                        .get(&(holder.routine.id, period.period_start.to_string()))
                        .copied()
                        .unwrap_or(0);
                    if done >= period.required {
                        continue;
                    }
                    let today_in_period = today >= period.period_start
                        && today <= period.period_end
                        && today >= week_start
                        && today <= week_end;
                    let cell_date = if today_in_period {
                        today
                    } else {
                        period.period_start.max(week_start)
                    };
                    if db_keys.contains(&(holder.routine.id, cell_date)) {
                        continue;
                    }
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
                todo_kind: todo.kind,
                all_day: todo.all_day,
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
    return Ok(days);
}

async fn week_view(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Query(query): Query<WeekQuery>,
    headers: axum::http::HeaderMap,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    let anchor = parse_date_param(query.date.as_deref(), today)?;
    let who = resolve_who(&state.pool, query.who.as_deref(), Who::Me)
        .await?;
    let shell = Shell::load(&state, &auth, "calendar", &headers).await?;
    let (week_start, week_end, days) = build_week(
        &state.pool,
        &auth.timezone,
        anchor,
        today,
        who.filter_id(auth.id),
        true,
    )
    .await?;
    let members = store::list_members(&state.pool).await?;
    let goal_data = load_goal_data(&state.pool).await?;
    let hidden = weekly_goal_ids(&goal_data);
    let goals = goal_bars(
        &goal_data,
        who.filter_id(auth.id),
        auth.id,
        matches!(who, Who::All),
        today,
        week_start,
        week_end,
    );
    let page = build_page(
        shell,
        PageInput {
            me: auth.id,
            who,
            week_start,
            week_end,
            days,
            members: &members,
            today,
            now_min: day::local_now_minutes(&auth.timezone),
            goals,
            hidden,
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
        false,
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
    for member in input.members {
        let avatar = AvatarVm::for_user(
            member.id,
            &member.username,
            member.avatar_path.as_deref(),
            member.avatar_icon.as_deref(),
            member.avatar_color,
        );
        colors.insert(member.id, avatar.color);
    }
    let (prev_week, next_week) = neighbor_weeks(input.week_start, input.week_end);
    let suffix = match input.who {
        Who::Me => String::new(),
        Who::All => "?who=all".to_string(),
        Who::User(id) => format!("?who={id}"),
    };
    let amp_suffix = suffix.replacen('?', "&", 1);
    let selected = match input.who {
        Who::User(id) => Some(id),
        Who::Me => Some(input.me),
        Who::All => None,
    };
    let picker = build_picker("/week", input.me, selected, input.week_start, input.members);
    let who_q = match input.who {
        Who::User(id) => format!("&who={id}"),
        Who::All => "&who=all".to_string(),
        Who::Me => String::new(),
    };
    let all_mode = matches!(input.who, Who::All);
    let mut days: Vec<WeekDayVm> = Vec::new();
    for cell in input.days {
        days.push(build_day_vm(
            cell,
            input.today,
            all_mode,
            &colors,
            input.me,
            &input.hidden,
        ));
    }
    let (grid_start, grid_end) = grid_range(&days);
    let span = (grid_end - grid_start).max(60);
    let cols: Vec<GridColVm> = days
        .iter()
        .map(|vm| build_grid_col(vm, grid_start, span, input.today, input.now_min))
        .collect();
    let hours: Vec<String> = (grid_start / 60..grid_end / 60)
        .map(|hour| format!("{hour:02}:00"))
        .collect();
    let has_all_day = cols.iter().any(|col| !col.all_day.is_empty());
    let week_has_today = input.today >= input.week_start && input.today <= input.week_end;
    let (past_days, days): (Vec<WeekDayVm>, Vec<WeekDayVm>) = if week_has_today {
        days.into_iter().partition(|vm| vm.is_past)
    } else {
        (Vec::new(), days)
    };
    let who_param = match input.who {
        Who::Me => "me".to_string(),
        Who::All => "all".to_string(),
        Who::User(id) => id.to_string(),
    };
    let has_entries = !input.goals.is_empty()
        || days.iter().chain(past_days.iter()).any(|vm| !vm.is_empty);
    return WeekPage {
        shell,
        week_start: input.week_start,
        week_end: input.week_end,
        prev_href: format!("/week?date={prev_week}{amp_suffix}"),
        next_href: format!("/week?date={next_week}{amp_suffix}"),
        this_week_href: format!("/week{suffix}"),
        picker,
        days,
        past_days,
        cols,
        grid_hours: span / 60,
        hours,
        has_all_day,
        goals: input.goals,
        who_param,
        who_q,
        switch_date: if input.today >= input.week_start && input.today <= input.week_end {
            input.today
        } else {
            input.week_start
        },
        has_entries,
    };
}

fn day_href(date: NaiveDate, owner: i64, me: i64) -> String {
    if owner == me {
        return format!("/day?date={date}");
    }
    return format!("/day?date={date}&who={owner}");
}

fn build_day_vm(
    cell: DayCell,
    today: NaiveDate,
    all_mode: bool,
    colors: &HashMap<i64, u8>,
    me: i64,
    hidden: &HashSet<i64>,
) -> WeekDayVm {
    let date = cell.date;
    let mut flagged: Vec<(i64, ItemVm)> = Vec::new();
    let mut goals: Vec<ItemVm> = Vec::new();
    let mut timed: Vec<(i64, ItemVm)> = Vec::new();
    let mut untimed: Vec<ItemVm> = Vec::new();
    for entry in cell.entries {
        match entry {
            WeekEntry::Occurrence(e) => {
                if hidden.contains(&e.routine_id) {
                    continue;
                }
                let start_min = day::parse_time_minutes(e.time_of_day.as_deref());
                let end_min = day::parse_time_minutes(e.time_end.as_deref());
                let owner = owner_vm(all_mode, e.user_id, &e.username, colors);
                let (pill_class, pill_icon, pill_word) = occurrence_pill(e.status, e.planned);
                let item = ItemVm {
                    kind: "routine",
                    href: if e.user_id == me {
                        format!("/routines/{}/edit", e.routine_id)
                    } else {
                        day_href(date, e.user_id, me)
                    },
                    routine_id: Some(e.routine_id),
                    todo_id: None,
                    status: e.status,
                    planned: e.planned,
                    title: e.title,
                    time_label: e.time_label.clone().unwrap_or_default(),
                    start_label: e.time_of_day.clone().unwrap_or_default(),
                    count_label: None,
                    pill_class,
                    pill_word,
                    pill_icon,
                    has_pill: true,
                    owner,
                    occurrence_id: e.occurrence_id,
                    has_pin: e.pin.is_some() && e.occurrence_id.is_some(),
                    done: e.status == "completed",
                    is_goal: e.is_goal,
                    can_tick: e.user_id == me && date == today,
                    start_min: start_min.unwrap_or(0),
                    end_min: start_min
                        .map(|start| start + day::block_len(start, end_min))
                        .unwrap_or(0),
                    grid_style: String::new(),
                };
                if e.is_goal && e.status == "completed" {
                    goals.push(item);
                    continue;
                }
                match start_min {
                    Some(start_min) => timed.push((start_min, item)),
                    None => untimed.push(item),
                }
            }
            WeekEntry::Period(p) => {
                untimed.push(ItemVm {
                    kind: "period",
                    href: if p.user_id == me {
                        format!("/routines/{}/edit", p.routine_id)
                    } else {
                        day_href(date, p.user_id, me)
                    },
                    routine_id: None,
                    todo_id: None,
                    status: "open",
                    planned: false,
                    title: p.title,
                    time_label: String::new(),
                    start_label: String::new(),
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
                    done: false,
                    is_goal: true,
                    can_tick: false,
                    start_min: 0,
                    end_min: 0,
                    grid_style: String::new(),
                });
            }
            WeekEntry::Todo(t) => {
                let start_min = day::parse_time_minutes(Some(&t.local_time)).unwrap_or(0);
                let is_deadline = t.todo_kind == "deadline";
                let (status, pill_class, pill_word, pill_icon) = if t.completed {
                    ("completed", "pill-done", "done", "circle-check")
                } else if t.overdue {
                    ("missed", "pill-missed", "overdue", "circle-alert")
                } else {
                    ("open", "pill-open", "open", "circle-dashed")
                };
                let all_day = t.all_day;
                let start_label = if all_day { String::new() } else { t.local_time.clone() };
                let item = ItemVm {
                    kind: if is_deadline { "deadline" } else { "todo" },
                    href: day_href(date, t.user_id, me),
                    routine_id: None,
                    todo_id: Some(t.todo_id),
                    status,
                    planned: false,
                    title: t.title,
                    time_label: start_label.clone(),
                    start_label,
                    count_label: None,
                    pill_class,
                    pill_word,
                    pill_icon,
                    has_pill: true,
                    owner: owner_vm(all_mode, t.user_id, &t.username, colors),
                    occurrence_id: None,
                    has_pin: false,
                    done: t.completed,
                    is_goal: false,
                    can_tick: t.user_id == me,
                    start_min,
                    end_min: start_min + 30,
                    grid_style: String::new(),
                };
                if is_deadline {
                    flagged.push((if all_day { -1 } else { start_min }, item));
                } else if all_day {
                    untimed.push(item);
                } else {
                    timed.push((start_min, item));
                }
            }
        }
    }
    flagged.sort_by_key(|(start, _)| *start);
    timed.sort_by_key(|(start, _)| *start);
    let mut all_day: Vec<ItemVm> = flagged.into_iter().map(|(_, item)| item).collect();
    all_day.extend(goals);
    all_day.extend(untimed);
    let timed: Vec<ItemVm> = timed.into_iter().map(|(_, item)| item).collect();
    let flagged_count = all_day.iter().filter(|item| item.kind == "deadline").count();
    let mut agenda: Vec<ItemVm> = all_day[..flagged_count].to_vec();
    agenda.extend(timed.iter().cloned());
    agenda.extend(all_day[flagged_count..].iter().cloned());
    let is_empty = agenda.is_empty();
    return WeekDayVm {
        date: cell.date,
        weekday_label: cell.date.format("%a").to_string(),
        day_num: cell.date.day(),
        is_today: cell.date == today,
        is_past: cell.date < today,
        agenda,
        all_day,
        timed,
        is_empty,
    };
}

fn grid_range(days: &[WeekDayVm]) -> (i64, i64) {
    let mut start = GRID_DEFAULT_START;
    let mut end = GRID_DEFAULT_END;
    for item in days.iter().flat_map(|vm| vm.timed.iter()) {
        start = start.min((item.start_min / 60 - 1) * 60);
        end = end.max((item.end_min + 59) / 60 * 60 + 60);
    }
    return (start.max(0), end.min(24 * 60));
}

fn build_grid_col(
    vm: &WeekDayVm,
    grid_start: i64,
    span: i64,
    today: NaiveDate,
    now_min: i64,
) -> GridColVm {
    let spans: Vec<(i64, i64)> = vm
        .timed
        .iter()
        .map(|item| (item.start_min, item.end_min))
        .collect();
    let columns = day::overlap_columns(&spans);
    let percent = |minutes: i64| -> f64 {
        return (minutes - grid_start) as f64 * 100.0 / span as f64;
    };
    let blocks = vm
        .timed
        .iter()
        .zip(columns)
        .map(|(item, (column, total))| {
            let mut block = item.clone();
            let (left, width) = if total <= GRID_CASCADE_MAX {
                let left = f64::from(column - 1) * GRID_CASCADE_STEP;
                (left, 100.0 - left)
            } else {
                let width = 100.0 / f64::from(total);
                (f64::from(column - 1) * width, width)
            };
            block.grid_style = format!(
                "top:calc({:.3}% + 1px);height:calc({:.3}% - 2px);left:calc({left:.3}% + 2px);width:calc({width:.3}% - 4px);z-index:{column}",
                percent(item.start_min),
                percent(item.end_min) - percent(item.start_min),
            );
            if item.end_min - item.start_min < 45 {
                block.time_label = block.start_label.clone();
            }
            return block;
        })
        .collect();
    let now_top = (vm.date == today && now_min >= grid_start && now_min <= grid_start + span)
        .then(|| format!("{:.3}", percent(now_min)));
    return GridColVm {
        date: vm.date,
        weekday_label: vm.weekday_label.clone(),
        day_num: vm.day_num,
        is_today: vm.is_today,
        is_past: vm.is_past,
        all_day: vm.all_day.clone(),
        blocks,
        now_top,
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

#[derive(Template)]
#[template(path = "month.html")]
pub struct MonthPage {
    pub shell: Shell,
    month_label: String,
    prev_href: String,
    next_href: String,
    this_month_href: String,
    picker: PickerVm,
    weeks: Vec<MonthWeekVm>,
    who_param: String,
    who_q: String,
    switch_date: NaiveDate,
}

struct MonthWeekVm {
    href: String,
    label: String,
    days: Vec<MonthCellVm>,
    bars: Vec<GoalBarVm>,
}

pub(crate) struct GoalBarVm {
    href: String,
    title: String,
    tooltip: String,
    status: &'static str,
    pips: Vec<bool>,
    count_label: String,
    days_label: String,
    routine_id: i64,
    slots: Vec<GoalSlotVm>,
}

pub(crate) struct GoalSlotVm {
    date: NaiveDate,
    letter: String,
    state: &'static str,
    can_tick: bool,
    can_untick: bool,
}

fn weekly_goal_ids(data: &GoalData) -> HashSet<i64> {
    return data
        .holders
        .iter()
        .filter(|holder| {
            store::schedule_from_row(&holder.routine).is_some_and(|schedule| {
                schedule.kind == ScheduleKind::Count && schedule.period == PeriodKind::Week
            })
        })
        .map(|holder| holder.routine.id)
        .collect();
}

const MAX_BAR_PIPS: u32 = 7;

struct GoalData {
    holders: Vec<store::RoutineWithUser>,
    dates: HashMap<(i64, String), Vec<NaiveDate>>,
}

async fn load_goal_data(pool: &SqlitePool) -> Result<GoalData, sqlx::Error> {
    let holders = store::active_routines_with_users(pool).await?;
    let mut dates: HashMap<(i64, String), Vec<NaiveDate>> = HashMap::new();
    for (routine_id, period_start, due) in store::period_completion_dates(pool).await? {
        if let Ok(date) = NaiveDate::parse_from_str(&due, "%Y-%m-%d") {
            dates.entry((routine_id, period_start)).or_default().push(date);
        }
    }
    return Ok(GoalData { holders, dates });
}

fn goal_bars(
    data: &GoalData,
    who: Option<i64>,
    me: i64,
    all_mode: bool,
    today: NaiveDate,
    week_start: NaiveDate,
    week_end: NaiveDate,
) -> Vec<GoalBarVm> {
    let mut bars = Vec::new();
    for holder in &data.holders {
        if who.is_some_and(|id| holder.routine.user_id != id) {
            continue;
        }
        let Some(schedule) = store::schedule_from_row(&holder.routine) else {
            continue;
        };
        if schedule.kind != ScheduleKind::Count || schedule.period != PeriodKind::Week {
            continue;
        }
        for period in periods_in_range(&schedule, week_start, week_end) {
            let done_dates = data
                .dates
                .get(&(holder.routine.id, period.period_start.to_string()))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let done = u32::try_from(done_dates.len()).unwrap_or(0);
            let status = if done >= period.required {
                "completed"
            } else if period.period_end < today {
                "missed"
            } else {
                "open"
            };
            let pips = if period.required <= MAX_BAR_PIPS {
                (0..period.required).map(|index| index < done).collect()
            } else {
                Vec::new()
            };
            let tooltip = if all_mode {
                format!(
                    "{} ({}): {done} of {} this week",
                    holder.routine.title, holder.username, period.required
                )
            } else {
                format!(
                    "{}: {done} of {} this week",
                    holder.routine.title, period.required
                )
            };
            let mine = holder.routine.user_id == me;
            let mut slots = Vec::new();
            let mut cursor = week_start;
            while cursor <= week_end {
                let in_period = cursor >= period.period_start && cursor <= period.period_end;
                let done_here = done_dates.contains(&cursor);
                let state = if !in_period {
                    "off"
                } else if done_here {
                    "done"
                } else if cursor == today {
                    "today"
                } else if cursor > today && status == "open" {
                    "open"
                } else {
                    "idle"
                };
                slots.push(GoalSlotVm {
                    date: cursor,
                    letter: cursor.format("%a").to_string(),
                    state,
                    can_tick: mine && cursor == today && !done_here && status != "completed",
                    can_untick: mine && cursor == today && done_here,
                });
                cursor = match cursor.succ_opt() {
                    Some(next) => next,
                    None => break,
                };
            }
            bars.push(GoalBarVm {
                routine_id: holder.routine.id,
                slots,
                href: if holder.routine.user_id == me {
                    format!("/routines/{}/edit", holder.routine.id)
                } else {
                    format!(
                        "/week?date={}&who={}",
                        period.period_start, holder.routine.user_id
                    )
                },
                title: if all_mode {
                    format!("{} \u{b7} {}", holder.routine.title, holder.username)
                } else {
                    holder.routine.title.clone()
                },
                tooltip,
                status,
                pips,
                count_label: format!("{done}/{}", period.required),
                days_label: day::done_days_label(done_dates, PeriodKind::Week),
            });
        }
    }
    bars.sort_by(|a, b| a.title.cmp(&b.title));
    return bars;
}

struct MonthCellVm {
    date: NaiveDate,
    day_num: u32,
    in_month: bool,
    is_today: bool,
    total: usize,
    more: usize,
    items: Vec<ItemVm>,
    dots: Vec<&'static str>,
}

const MONTH_CELL_ITEMS: usize = 3;
const MONTH_CELL_DOTS: usize = 8;

fn month_bounds(anchor: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    let first = anchor.with_day(1)?;
    let next_first = if first.month() == 12 {
        NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1)?
    };
    return Some((first, next_first.pred_opt()?));
}

fn dot_class(item: &ItemVm) -> &'static str {
    if item.kind == "deadline" {
        return "dot-deadline";
    }
    return match item.status {
        "completed" => "dot-done",
        "missed" => "dot-missed",
        _ => "dot-open",
    };
}

async fn month_view(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Query(query): Query<WeekQuery>,
    headers: axum::http::HeaderMap,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    let anchor = parse_date_param(query.date.as_deref(), today)?;
    let who = resolve_who(&state.pool, query.who.as_deref(), Who::Me)
        .await?;
    let Some((first, last)) = month_bounds(anchor) else {
        return Err(WebError::Validation("invalid date"));
    };
    let grid_start = first
        .checked_sub_days(chrono::Days::new(u64::from(
            first.weekday().num_days_from_monday(),
        )))
        .unwrap_or(first);
    let grid_end = last
        .checked_add_days(chrono::Days::new(u64::from(
            6 - last.weekday().num_days_from_monday(),
        )))
        .unwrap_or(last);
    let shell = Shell::load(&state, &auth, "calendar", &headers).await?;
    let cells = build_cells(
        &state.pool,
        &auth.timezone,
        grid_start,
        grid_end,
        today,
        who.filter_id(auth.id),
        true,
    )
    .await?;
    let members = store::list_members(&state.pool).await?;
    let mut colors: HashMap<i64, u8> = HashMap::new();
    for member in &members {
        let avatar = AvatarVm::for_user(
            member.id,
            &member.username,
            member.avatar_path.as_deref(),
            member.avatar_icon.as_deref(),
            member.avatar_color,
        );
        colors.insert(member.id, avatar.color);
    }
    let all_mode = matches!(who, Who::All);
    let who_param = match who {
        Who::Me => "me".to_string(),
        Who::All => "all".to_string(),
        Who::User(id) => id.to_string(),
    };
    let goal_data = load_goal_data(&state.pool).await?;
    let mut weeks: Vec<MonthWeekVm> = Vec::new();
    let mut row: Vec<MonthCellVm> = Vec::new();
    for cell in cells {
        let vm = build_day_vm(cell, today, all_mode, &colors, auth.id, &HashSet::new());
        let total = vm.agenda.len();
        let dots = vm.agenda.iter().take(MONTH_CELL_DOTS).map(dot_class).collect();
        let items: Vec<ItemVm> = vm.agenda.into_iter().take(MONTH_CELL_ITEMS).collect();
        row.push(MonthCellVm {
            date: vm.date,
            day_num: vm.day_num,
            in_month: vm.date >= first && vm.date <= last,
            is_today: vm.is_today,
            total,
            more: total.saturating_sub(MONTH_CELL_ITEMS),
            items,
            dots,
        });
        if row.len() == 7 {
            let start = row[0].date;
            let end = row[6].date;
            let iso = start.iso_week().week();
            let bars = goal_bars(
                &goal_data,
                who.filter_id(auth.id),
                auth.id,
                all_mode,
                today,
                start,
                end,
            );
            weeks.push(MonthWeekVm {
                bars,
                href: format!("/week?date={start}&who={who_param}"),
                label: format!("W{iso}"),
                days: std::mem::take(&mut row),
            });
        }
    }
    let prev_first = first
        .pred_opt()
        .and_then(|date| date.with_day(1))
        .unwrap_or(first);
    let next_first = last.succ_opt().unwrap_or(last);
    let suffix = match who {
        Who::Me => String::new(),
        Who::All => "&who=all".to_string(),
        Who::User(id) => format!("&who={id}"),
    };
    let selected = match who {
        Who::User(id) => Some(id),
        Who::Me => Some(auth.id),
        Who::All => None,
    };
    let page = MonthPage {
        shell,
        month_label: first.format("%B %Y").to_string(),
        prev_href: format!("/month?date={prev_first}{suffix}"),
        next_href: format!("/month?date={next_first}{suffix}"),
        this_month_href: format!("/month{}", suffix.replacen('&', "?", 1)),
        picker: build_picker("/month", auth.id, selected, first, &members),
        weeks,
        who_param,
        who_q: suffix.clone(),
        switch_date: if today >= first && today <= last {
            today
        } else {
            first
        },
    };
    return Ok(html(&page)?);
}
