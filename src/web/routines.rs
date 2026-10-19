use askama::Template;
use axum::Router;
use axum::extract::Form;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::Redirect;
use axum::routing::get;
use axum::routing::post;
use chrono::NaiveDate;
use chrono::NaiveTime;
use chrono::Weekday;
use serde::Deserialize;
use serde::Serialize;

use crate::app::AppState;
use crate::auth::extract::ApiAuth;
use crate::auth::extract::PageAuth;
use crate::recurrence::is_finished_on;
use crate::recurrence::mask_contains;
use crate::recurrence::mask_from_weekdays;
use crate::score;
use crate::store;

use super::WebError;
use super::WebResult;
use super::html;
use super::notifications::RemindersVm;
use super::notifications::reminder_error_message;
use super::shell::AvatarVm;
use super::shell::Shell;
use super::week::PickerVm;

const ROUTINE_TIME_SOURCE: &str = "routine-time-of-day";

#[derive(Template)]
#[template(path = "routines.html")]
pub struct RoutinesPage {
    pub shell: Shell,
    picker: PickerVm,
    readonly: bool,
    active: Vec<RoutineCardVm>,
    finished: Vec<RoutineCardVm>,
    archived: Vec<RoutineCardVm>,
}

pub struct RoutineCardVm {
    pub id: i64,
    pub title: String,
    pub schedule: String,
    pub pct_30d: Option<String>,
}

#[derive(Template)]
#[template(path = "routine_view.html")]
pub struct RoutineViewPage {
    pub shell: Shell,
    owner_id: i64,
    owner_name: String,
    owner_avatar: AvatarVm,
    id: i64,
    title: String,
    notes: Option<String>,
    schedule: String,
    time_window: String,
    start_date: String,
    duration: String,
    finished: bool,
    archived: bool,
    can_edit: bool,
    edit_id: i64,
    back_href: String,
}

#[derive(Debug, Deserialize)]
struct RoutinesQuery {
    who: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct NewRoutineQuery {
    from: Option<i64>,
}

#[derive(Debug, Serialize)]
struct RoutineJson {
    id: i64,
    user_id: i64,
    username: String,
    title: String,
    notes: Option<String>,
    kind: String,
    weekdays: Option<Vec<u8>>,
    period: Option<&'static str>,
    per_period: Option<u32>,
    iterations: Option<u32>,
    start_date: Option<NaiveDate>,
    end_date: Option<NaiveDate>,
    time_of_day: Option<String>,
    time_end: Option<String>,
    archived: bool,
    finished: bool,
}

#[derive(Template)]
#[template(path = "new.html")]
pub struct NewRoutinePage {
    pub shell: Shell,
    form: RoutineFormVm,
}

#[derive(Template)]
#[template(path = "edit.html")]
pub struct EditRoutinePage {
    pub shell: Shell,
    form: RoutineFormVm,
}

struct RoutineFormVm {
    action: String,
    submit_label: &'static str,
    cancel: bool,
    error: Option<&'static str>,
    err_title: Option<&'static str>,
    err_kind: Option<&'static str>,
    err_weekdays: Option<&'static str>,
    err_per_period: Option<&'static str>,
    err_period: Option<&'static str>,
    err_iterations: Option<&'static str>,
    err_end_date: Option<&'static str>,
    err_start_date: Option<&'static str>,
    err_time_of_day: Option<&'static str>,
    err_time_end: Option<&'static str>,
    title: String,
    notes: String,
    kind_weekdays: bool,
    kind_count: bool,
    monday: bool,
    tuesday: bool,
    wednesday: bool,
    thursday: bool,
    friday: bool,
    saturday: bool,
    sunday: bool,
    per_period: String,
    period_day: bool,
    period_week: bool,
    period_month: bool,
    period_year: bool,
    duration_forever: bool,
    duration_iterations: bool,
    duration_end_date: bool,
    iterations: String,
    end_date: String,
    show_start: bool,
    start_date: String,
    time_of_day: String,
    time_end: String,
    reminders: RemindersVm,
}

impl RoutineFormVm {
    fn empty(action: String) -> RoutineFormVm {
        return RoutineFormVm {
            action,
            submit_label: "Create routine",
            cancel: false,
            error: None,
            err_title: None,
            err_kind: None,
            err_weekdays: None,
            err_per_period: None,
            err_period: None,
            err_iterations: None,
            err_end_date: None,
            err_start_date: None,
            err_time_of_day: None,
            err_time_end: None,
            title: String::new(),
            notes: String::new(),
            kind_weekdays: true,
            kind_count: false,
            monday: false,
            tuesday: false,
            wednesday: false,
            thursday: false,
            friday: false,
            saturday: false,
            sunday: false,
            per_period: String::new(),
            period_day: false,
            period_week: true,
            period_month: false,
            period_year: false,
            duration_forever: true,
            duration_iterations: false,
            duration_end_date: false,
            iterations: String::new(),
            end_date: String::new(),
            show_start: true,
            start_date: String::new(),
            time_of_day: String::new(),
            time_end: String::new(),
            reminders: RemindersVm::new(&[], false, ROUTINE_TIME_SOURCE),
        };
    }

    fn apply_errors(&mut self, errors: &[FieldError]) {
        for error in errors {
            match error.field {
                "title" => self.err_title = Some(error.ui_message),
                "kind" => self.err_kind = Some(error.ui_message),
                "weekdays" => self.err_weekdays = Some(error.ui_message),
                "per_period" => self.err_per_period = Some(error.ui_message),
                "period" => self.err_period = Some(error.ui_message),
                "iterations" => self.err_iterations = Some(error.ui_message),
                "end_date" => self.err_end_date = Some(error.ui_message),
                "start_date" => self.err_start_date = Some(error.ui_message),
                "time_of_day" => self.err_time_of_day = Some(error.ui_message),
                "time_end" => self.err_time_end = Some(error.ui_message),
                _ => {}
            }
        }
        self.error = if errors.is_empty() {
            None
        } else {
            Some("Please fix the highlighted fields.")
        };
    }
}

pub(crate) struct FieldError {
    pub field: &'static str,
    pub message: &'static str,
    pub ui_message: &'static str,
}

#[derive(Debug, Deserialize)]
struct RoutineFormPayload {
    title: Option<String>,
    notes: Option<String>,
    kind: Option<String>,
    monday: Option<String>,
    tuesday: Option<String>,
    wednesday: Option<String>,
    thursday: Option<String>,
    friday: Option<String>,
    saturday: Option<String>,
    sunday: Option<String>,
    per_period: Option<String>,
    period: Option<String>,
    duration: Option<String>,
    iterations: Option<String>,
    end_date: Option<String>,
    start_date: Option<String>,
    time_of_day: Option<String>,
    time_end: Option<String>,
    reminders_json: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RoutineJsonPayload {
    title: Option<String>,
    notes: Option<String>,
    kind: Option<String>,
    weekdays: Option<Vec<u8>>,
    per_period: Option<u32>,
    period: Option<String>,
    iterations: Option<u32>,
    end_date: Option<NaiveDate>,
    start_date: Option<NaiveDate>,
    time_of_day: Option<String>,
    time_end: Option<String>,
    reminders: Option<Vec<store::ReminderSpec>>,
}

#[derive(Debug, Serialize)]
struct RoutineCreated {
    id: i64,
}

pub fn router() -> Router<AppState> {
    return Router::new()
        .route("/routines", get(routines_page).post(create_routine_ssr))
        .route("/routines/new", get(new_routine_page))
        .route("/routines/{id}/edit", get(edit_routine_page))
        .route(
            "/routines/{id}",
            get(routine_view_page).post(update_routine_ssr),
        )
        .route("/routines/{id}/archive", post(archive_ssr))
        .route("/routines/{id}/restore", post(restore_ssr))
        .route("/routines/{id}/delete", post(delete_ssr))
        .route("/api/v1/routines", post(api_create_routine))
        .route(
            "/api/v1/routines/{id}",
            get(api_get_routine).put(api_update_routine),
        )
        .route("/api/v1/routines/{id}/archive", post(api_archive))
        .route("/api/v1/routines/{id}/restore", post(api_restore))
        .route("/api/v1/routines/{id}/delete", post(api_delete));
}

async fn routines_page(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Query(query): Query<RoutinesQuery>,
    headers: axum::http::HeaderMap,
) -> WebResult {
    let shell = Shell::load(&state, &auth, "routines", &headers).await?;
    let members = store::list_members(&state.pool).await?;
    let target = query.who.unwrap_or(auth.id);
    if !members.iter().any(|member| member.id == target) {
        return Err(WebError::NotFound);
    }
    let readonly = target != auth.id;
    let picker = super::week::build_picker_with(
        "/routines",
        auth.id,
        Some(target),
        &members,
        "routines",
        |who| match who {
            Some(id) => format!("who={id}"),
            None => String::new(),
        },
    );
    let mut conn = state.pool.acquire().await?;
    let heads = store::routine_heads(&mut conn, target).await?;
    drop(conn);
    let today = store::user_today(&auth.timezone);
    let pcts = if readonly {
        vec![None; heads.len()]
    } else {
        score::routine_window_pcts(&state.pool, auth.id, &auth.timezone, &heads).await?
    };
    let mut active = Vec::new();
    let mut finished = Vec::new();
    for (routine, pct) in heads.iter().zip(pcts) {
        let Some(schedule) = store::schedule_from_row(routine) else {
            continue;
        };
        let card = RoutineCardVm {
            id: routine.id,
            title: routine.title.clone(),
            schedule: score::describe_schedule(
                &schedule,
                routine.time_of_day.as_deref(),
                routine.time_end.as_deref(),
                today,
            ),
            pct_30d: pct.map(pct_label),
        };
        if is_finished_on(&schedule, today) {
            finished.push(card);
        } else {
            active.push(card);
        }
    }
    let mut archived = Vec::new();
    if !readonly {
        let archived_rows = store::archived_routine_heads(&state.pool, auth.id).await?;
        for routine in &archived_rows {
            let Some(schedule) = store::schedule_from_row(routine) else {
                continue;
            };
            archived.push(RoutineCardVm {
                id: routine.id,
                title: routine.title.clone(),
                schedule: score::describe_schedule(
                    &schedule,
                    routine.time_of_day.as_deref(),
                    routine.time_end.as_deref(),
                    today,
                ),
                pct_30d: None,
            });
        }
    }
    return Ok(html(&RoutinesPage {
        shell,
        picker,
        readonly,
        active,
        finished,
        archived,
    })?);
}

async fn routine_view_page(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
    headers: axum::http::HeaderMap,
) -> WebResult {
    let Some(visible) = store::routine_visible_to(&state.pool, auth.id, routine_id).await? else {
        return Err(WebError::NotFound);
    };
    let shell = Shell::load(&state, &auth, "routines", &headers).await?;
    let routine = &visible.routine;
    let Some(schedule) = store::schedule_from_row(routine) else {
        return Err(WebError::NotFound);
    };
    let today = store::user_today(&auth.timezone);
    let owner_avatar = AvatarVm::load(&state.pool, routine.user_id).await?;
    let own = routine.user_id == auth.id;
    let duration = match (routine.iterations, routine.end_date) {
        (Some(weeks), _) => format!("{weeks} {}", if weeks == 1 { "week" } else { "weeks" }),
        (None, Some(end)) => format!("Until {end}"),
        (None, None) => "Forever".to_string(),
    };
    let time_window = match (routine.time_of_day.as_deref(), routine.time_end.as_deref()) {
        (Some(start), Some(end)) => format!("{start} \u{2013} {end}"),
        (Some(start), None) => start.to_string(),
        (None, _) => "All day".to_string(),
    };
    let page = RoutineViewPage {
        shell,
        owner_id: routine.user_id,
        owner_name: visible.username.clone(),
        owner_avatar,
        id: routine.id,
        title: routine.title.clone(),
        notes: routine
            .notes
            .clone()
            .filter(|notes| !notes.trim().is_empty()),
        schedule: score::describe_schedule(
            &schedule,
            routine.time_of_day.as_deref(),
            routine.time_end.as_deref(),
            today,
        ),
        time_window,
        start_date: schedule.start_date.to_string(),
        duration,
        finished: is_finished_on(&schedule, today),
        archived: visible.archived,
        can_edit: own && !visible.archived,
        edit_id: visible.head_id,
        back_href: if own {
            "/routines".to_string()
        } else {
            format!("/routines?who={}", routine.user_id)
        },
    };
    return Ok(html(&page)?);
}

async fn api_get_routine(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
) -> Result<axum::Json<RoutineJson>, WebError> {
    let Some(visible) = store::routine_visible_to(&state.pool, auth.id, routine_id).await? else {
        return Err(WebError::NotFound);
    };
    let routine = visible.routine;
    let today = store::user_today(&auth.timezone);
    let finished = store::schedule_from_row(&routine)
        .map(|schedule| is_finished_on(&schedule, today))
        .unwrap_or(false);
    let weekdays = routine.weekdays_mask.map(|mask| {
        return (0u8..7)
            .filter(|index| {
                weekday_from_index(*index).is_some_and(|day| mask_contains(mask, day))
            })
            .collect::<Vec<u8>>();
    });
    let period = routine.period.map(|value| match value {
        store::PeriodKind::Day => "day",
        store::PeriodKind::Week => "week",
        store::PeriodKind::Month => "month",
        store::PeriodKind::Year => "year",
    });
    return Ok(axum::Json(RoutineJson {
        id: routine.id,
        user_id: routine.user_id,
        username: visible.username,
        title: routine.title,
        notes: routine.notes,
        kind: routine.kind,
        weekdays,
        period,
        per_period: routine.per_period,
        iterations: routine.iterations,
        start_date: routine.start_date,
        end_date: routine.end_date,
        time_of_day: routine.time_of_day,
        time_end: routine.time_end,
        archived: visible.archived,
        finished,
    }));
}

fn pct_label(value: f64) -> String {
    return format!("{}%", (value * 100.0).round() as i64);
}

async fn new_routine_page(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Query(query): Query<NewRoutineQuery>,
    headers: axum::http::HeaderMap,
) -> WebResult {
    let prefs = crate::notify::get_prefs(&state.pool, auth.id).await?;
    let mut form = RoutineFormVm::empty("/routines".to_string());
    let mut timed = false;
    if let Some(from) = query.from {
        let Some(visible) = store::routine_visible_to(&state.pool, auth.id, from).await? else {
            return Err(WebError::NotFound);
        };
        let row = visible.routine;
        form = form_from_row(&row, "/routines".to_string());
        form.submit_label = "Create routine";
        form.cancel = false;
        form.show_start = true;
        let today = store::user_today(&auth.timezone);
        if row.end_date.is_some_and(|end| end < today) {
            form.duration_end_date = false;
            form.duration_iterations = false;
            form.duration_forever = true;
            form.end_date = String::new();
        }
        timed = row.time_of_day.is_some();
    }
    let shell = Shell::load(&state, &auth, "routines", &headers).await?;
    form.reminders = RemindersVm::new(
        &store::default_reminder_specs(&prefs, "routine", timed),
        timed,
        ROUTINE_TIME_SOURCE,
    )
    .with_defaults(&prefs, "routine");
    return Ok(html(&NewRoutinePage { shell, form })?);
}

async fn create_routine_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Form(payload): Form<RoutineFormPayload>,
) -> WebResult {
    let today = store::user_today(&auth.timezone);
    match form_spec(&payload, today) {
        Ok(spec) => {
            let reminders = match form_reminders(payload.reminders_json.as_deref(), &spec) {
                Ok(reminders) => reminders,
                Err(message) => {
                    let shell = Shell::load(&state, &auth, "routines", &headers).await?;
                    let mut form = form_from_payload(&payload, "/routines".to_string());
                    form.reminders = form.reminders.with_error(message);
                    return Ok(html(&NewRoutinePage { shell, form })?);
                }
            };
            let id = store::create_routine(&state.pool, auth.id, &spec).await?;
            match reminders {
                Some(list) => {
                    store::replace_reminders(&state.pool, auth.id, "routine", id, &list).await?;
                }
                None => {
                    let timed = spec.time_of_day.is_some();
                    store::apply_default_reminders(&state.pool, auth.id, "routine", id, timed)
                        .await?;
                }
            }
            return Ok(super::shell::redirect_with_flash(
                "/routines",
                "Routine created",
                state.config.cookie_secure,
            ));
        }
        Err(errors) => {
            let shell = Shell::load(&state, &auth, "routines", &headers).await?;
            let mut form = form_from_payload(&payload, "/routines".to_string());
            form.apply_errors(&errors);
            return Ok(html(&NewRoutinePage { shell, form })?);
        }
    }
}

async fn edit_routine_page(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
    headers: axum::http::HeaderMap,
) -> WebResult {
    let shell = Shell::load(&state, &auth, "routines", &headers).await?;
    let Some(row) = store::routine_for_user(&state.pool, auth.id, routine_id).await? else {
        return Err(WebError::NotFound);
    };
    if row.archived_at.is_some() {
        return Err(WebError::Conflict);
    }
    let head_id = store::chain_head_id(&state.pool, routine_id).await?;
    if head_id != routine_id {
        return Ok(Redirect::to(&format!("/routines/{head_id}/edit")).into_response());
    }
    let mut form = form_from_row(&row, format!("/routines/{routine_id}"));
    let stored = store::list_reminders_for(&state.pool, auth.id, "routine", &[routine_id]).await?;
    form.reminders = RemindersVm::from_rows(&stored, row.time_of_day.is_some(), ROUTINE_TIME_SOURCE);
    return Ok(html(&EditRoutinePage { shell, form })?);
}

async fn update_routine_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
    headers: axum::http::HeaderMap,
    Form(payload): Form<RoutineFormPayload>,
) -> WebResult {
    let Some(row) = store::routine_for_user(&state.pool, auth.id, routine_id).await? else {
        return Err(WebError::NotFound);
    };
    if row.archived_at.is_some() || !store::is_chain_head(&state.pool, routine_id).await? {
        return Err(WebError::Conflict);
    }
    let today = store::user_today(&auth.timezone);
    match form_spec(&payload, today) {
        Ok(spec) => {
            let reminders = match form_reminders(payload.reminders_json.as_deref(), &spec) {
                Ok(reminders) => reminders,
                Err(message) => {
                    let shell = Shell::load(&state, &auth, "routines", &headers).await?;
                    let mut form = edit_form_from_payload(&payload, routine_id);
                    form.reminders = form.reminders.with_error(message);
                    return Ok(html(&EditRoutinePage { shell, form })?);
                }
            };
            match store::update_routine(&state.pool, auth.id, routine_id, &spec, today).await {
                Ok(head) => {
                    if let Some(list) = reminders {
                        store::replace_reminders(&state.pool, auth.id, "routine", head, &list)
                            .await
                            .map_err(WebError::from)?;
                    }
                    return Ok(super::shell::redirect_with_flash(
                        "/routines",
                        "Routine updated",
                        state.config.cookie_secure,
                    ));
                }
                Err(store::UpdateRoutineError::NotFound) => return Err(WebError::NotFound),
                Err(store::UpdateRoutineError::Conflict) => return Err(WebError::Conflict),
                Err(store::UpdateRoutineError::Db(error)) => return Err(WebError::Db(error)),
            }
        }
        Err(errors) => {
            let shell = Shell::load(&state, &auth, "routines", &headers).await?;
            let mut form = edit_form_from_payload(&payload, routine_id);
            form.apply_errors(&errors);
            if payload.reminders_json.is_none() {
                let stored =
                    store::list_reminders_for(&state.pool, auth.id, "routine", &[routine_id])
                        .await?;
                form.reminders = RemindersVm::from_rows(
                    &stored,
                    !payload.time_of_day.as_deref().unwrap_or("").trim().is_empty(),
                    ROUTINE_TIME_SOURCE,
                );
            }
            return Ok(html(&EditRoutinePage { shell, form })?);
        }
    }
}

fn edit_form_from_payload(payload: &RoutineFormPayload, routine_id: i64) -> RoutineFormVm {
    let mut form = form_from_payload(payload, format!("/routines/{routine_id}"));
    form.submit_label = "Save changes";
    form.cancel = true;
    form.show_start = false;
    return form;
}

async fn archive_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
) -> WebResult {
    let archived = store::archive_routine(&state.pool, auth.id, routine_id).await?;
    if !archived {
        return Err(WebError::NotFound);
    }
    return Ok(super::shell::redirect_with_flash(
        "/routines",
        "Routine archived",
        state.config.cookie_secure,
    ));
}

async fn restore_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
) -> WebResult {
    let restored = store::restore_routine(&state.pool, auth.id, routine_id).await?;
    if !restored {
        return Err(WebError::NotFound);
    }
    return Ok(super::shell::redirect_with_flash(
        "/routines",
        "Routine restored",
        state.config.cookie_secure,
    ));
}

async fn delete_ssr(
    PageAuth(auth): PageAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
) -> WebResult {
    match store::delete_archived_routine(&state.pool, auth.id, routine_id).await? {
        store::DeleteRoutineOutcome::Deleted => {
            return Ok(super::shell::redirect_with_flash(
                "/routines",
                "Routine deleted",
                state.config.cookie_secure,
            ));
        }
        store::DeleteRoutineOutcome::NotFound => return Err(WebError::NotFound),
        store::DeleteRoutineOutcome::NotArchived => return Err(WebError::Conflict),
    }
}

async fn api_create_routine(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    axum::Json(payload): axum::Json<RoutineJsonPayload>,
) -> Result<axum::Json<RoutineCreated>, WebError> {
    let today = store::user_today(&auth.timezone);
    let raw = json_raw(&payload)?;
    let spec = build_spec(raw, today).map_err(validation_error)?;
    if let Some(reminders) = &payload.reminders {
        store::normalize_reminders(reminders, spec.time_of_day.is_some())?;
    }
    let id = store::create_routine(&state.pool, auth.id, &spec).await?;
    match &payload.reminders {
        Some(reminders) => {
            store::replace_reminders(&state.pool, auth.id, "routine", id, reminders).await?;
        }
        None => {
            let timed = spec.time_of_day.is_some();
            store::apply_default_reminders(&state.pool, auth.id, "routine", id, timed).await?;
        }
    }
    return Ok(axum::Json(RoutineCreated { id }));
}

async fn api_update_routine(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
    axum::Json(payload): axum::Json<RoutineJsonPayload>,
) -> Result<axum::Json<RoutineCreated>, WebError> {
    let today = store::user_today(&auth.timezone);
    let raw = json_raw(&payload)?;
    let spec = build_spec(raw, today).map_err(validation_error)?;
    if let Some(reminders) = &payload.reminders {
        store::normalize_reminders(reminders, spec.time_of_day.is_some())?;
    }
    let head = match store::update_routine(&state.pool, auth.id, routine_id, &spec, today).await {
        Ok(head) => head,
        Err(store::UpdateRoutineError::NotFound) => return Err(WebError::NotFound),
        Err(store::UpdateRoutineError::Conflict) => return Err(WebError::Conflict),
        Err(store::UpdateRoutineError::Db(error)) => return Err(WebError::Db(error)),
    };
    if let Some(reminders) = &payload.reminders {
        store::replace_reminders(&state.pool, auth.id, "routine", head, reminders).await?;
    }
    return Ok(axum::Json(RoutineCreated { id: head }));
}

fn form_reminders(
    raw: Option<&str>,
    spec: &store::RoutineSpec,
) -> Result<Option<Vec<store::ReminderSpec>>, &'static str> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let list: Vec<store::ReminderSpec> =
        serde_json::from_str(raw).map_err(|_| "invalid reminders")?;
    store::normalize_reminders(&list, spec.time_of_day.is_some())
        .map_err(|error| reminder_error_message(&error))?;
    return Ok(Some(list));
}

fn validation_error(errors: Vec<FieldError>) -> WebError {
    let message = errors
        .first()
        .map(|error| error.message)
        .unwrap_or("invalid routine");
    return WebError::Validation(message);
}

async fn api_archive(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
) -> Result<axum::response::Response, WebError> {
    let archived = store::archive_routine(&state.pool, auth.id, routine_id).await?;
    if !archived {
        return Err(WebError::NotFound);
    }
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true })),
    )
        .into_response());
}

async fn api_restore(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
) -> Result<axum::response::Response, WebError> {
    let restored = store::restore_routine(&state.pool, auth.id, routine_id).await?;
    if !restored {
        return Err(WebError::NotFound);
    }
    return Ok((
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true })),
    )
        .into_response());
}

async fn api_delete(
    ApiAuth(auth): ApiAuth,
    State(state): State<AppState>,
    Path(routine_id): Path<i64>,
) -> Result<axum::response::Response, WebError> {
    match store::delete_archived_routine(&state.pool, auth.id, routine_id).await? {
        store::DeleteRoutineOutcome::Deleted => {
            return Ok((
                axum::http::StatusCode::OK,
                axum::Json(serde_json::json!({ "ok": true })),
            )
                .into_response());
        }
        store::DeleteRoutineOutcome::NotFound => return Err(WebError::NotFound),
        store::DeleteRoutineOutcome::NotArchived => return Err(WebError::Conflict),
    }
}

fn weekday_flags(mask: u8) -> (bool, bool, bool, bool, bool, bool, bool) {
    return (
        mask_contains(mask, Weekday::Mon),
        mask_contains(mask, Weekday::Tue),
        mask_contains(mask, Weekday::Wed),
        mask_contains(mask, Weekday::Thu),
        mask_contains(mask, Weekday::Fri),
        mask_contains(mask, Weekday::Sat),
        mask_contains(mask, Weekday::Sun),
    );
}

fn form_from_payload(payload: &RoutineFormPayload, action: String) -> RoutineFormVm {
    let mut form = RoutineFormVm::empty(action);
    form.title = payload.title.clone().unwrap_or_default();
    form.notes = payload.notes.clone().unwrap_or_default();
    match payload.kind.as_deref() {
        Some("count") => {
            form.kind_weekdays = false;
            form.kind_count = true;
        }
        _ => {
            form.kind_weekdays = true;
            form.kind_count = false;
        }
    }
    let submitted = [
        payload.monday.is_some(),
        payload.tuesday.is_some(),
        payload.wednesday.is_some(),
        payload.thursday.is_some(),
        payload.friday.is_some(),
        payload.saturday.is_some(),
        payload.sunday.is_some(),
    ];
    if submitted.iter().any(|day| *day) {
        (
            form.monday,
            form.tuesday,
            form.wednesday,
            form.thursday,
            form.friday,
            form.saturday,
            form.sunday,
        ) = (
            submitted[0],
            submitted[1],
            submitted[2],
            submitted[3],
            submitted[4],
            submitted[5],
            submitted[6],
        );
    }
    form.per_period = payload.per_period.clone().unwrap_or_default();
    match payload.period.as_deref() {
        Some("day") => {
            form.period_week = false;
            form.period_day = true;
        }
        Some("month") => {
            form.period_week = false;
            form.period_month = true;
        }
        Some("year") => {
            form.period_week = false;
            form.period_year = true;
        }
        _ => {}
    }
    match payload.duration.as_deref() {
        Some("iterations") => {
            form.duration_forever = false;
            form.duration_iterations = true;
        }
        Some("end_date") => {
            form.duration_forever = false;
            form.duration_end_date = true;
        }
        _ => {
            form.duration_forever = true;
        }
    }
    form.iterations = payload.iterations.clone().unwrap_or_default();
    form.end_date = payload.end_date.clone().unwrap_or_default();
    form.start_date = payload.start_date.clone().unwrap_or_default();
    form.time_of_day = payload.time_of_day.clone().unwrap_or_default();
    form.time_end = payload.time_end.clone().unwrap_or_default();
    form.reminders = RemindersVm::from_json(
        payload.reminders_json.as_deref(),
        !form.time_of_day.trim().is_empty(),
        ROUTINE_TIME_SOURCE,
    );
    return form;
}

fn form_from_row(row: &store::RoutineRow, action: String) -> RoutineFormVm {
    let mut form = RoutineFormVm::empty(action);
    form.submit_label = "Save changes";
    form.cancel = true;
    form.show_start = false;
    form.title = row.title.clone();
    form.notes = row.notes.clone().unwrap_or_default();
    match row.kind.as_str() {
        "count" => {
            form.kind_weekdays = false;
            form.kind_count = true;
        }
        _ => {
            form.kind_weekdays = true;
            form.kind_count = false;
        }
    }
    (
        form.monday,
        form.tuesday,
        form.wednesday,
        form.thursday,
        form.friday,
        form.saturday,
        form.sunday,
    ) = weekday_flags(row.weekdays_mask.unwrap_or(0));
    form.per_period = row
        .per_period
        .map(|value| value.to_string())
        .unwrap_or_default();
    match row.period {
        Some(store::PeriodKind::Day) => {
            form.period_week = false;
            form.period_day = true;
        }
        Some(store::PeriodKind::Month) => {
            form.period_week = false;
            form.period_month = true;
        }
        Some(store::PeriodKind::Year) => {
            form.period_week = false;
            form.period_year = true;
        }
        _ => {}
    }
    if row.iterations.is_some() {
        form.duration_iterations = true;
        form.duration_forever = false;
    } else if row.end_date.is_some() {
        form.duration_end_date = true;
        form.duration_forever = false;
    }
    form.iterations = row
        .iterations
        .map(|value| value.to_string())
        .unwrap_or_default();
    form.end_date = row
        .end_date
        .map(|value| value.to_string())
        .unwrap_or_default();
    form.time_of_day = row.time_of_day.clone().unwrap_or_default();
    form.time_end = row.time_end.clone().unwrap_or_default();
    return form;
}

fn form_spec(
    payload: &RoutineFormPayload,
    today: NaiveDate,
) -> Result<store::RoutineSpec, Vec<FieldError>> {
    let mut errors = Vec::new();
    let mut weekdays = Vec::new();
    if payload.monday.is_some() {
        weekdays.push(Weekday::Mon);
    }
    if payload.tuesday.is_some() {
        weekdays.push(Weekday::Tue);
    }
    if payload.wednesday.is_some() {
        weekdays.push(Weekday::Wed);
    }
    if payload.thursday.is_some() {
        weekdays.push(Weekday::Thu);
    }
    if payload.friday.is_some() {
        weekdays.push(Weekday::Fri);
    }
    if payload.saturday.is_some() {
        weekdays.push(Weekday::Sat);
    }
    if payload.sunday.is_some() {
        weekdays.push(Weekday::Sun);
    }
    let per_period = match payload.per_period.as_deref() {
        None | Some("") => None,
        Some(raw) => match raw.trim().parse::<u32>() {
            Ok(value) => Some(value),
            Err(_) => {
                errors.push(FieldError {
                    field: "per_period",
                    message: "per_period must be a positive number",
                    ui_message: "Enter how many times",
                });
                None
            }
        },
    };
    let period = match payload.period.as_deref() {
        None | Some("") => None,
        Some(raw) => match store::parse_period(raw.trim()) {
            Some(value) => Some(value),
            None => {
                errors.push(FieldError {
                    field: "period",
                    message: "unknown period",
                    ui_message: "Choose a period",
                });
                None
            }
        },
    };
    let (iterations, end_date) = match payload.duration.as_deref() {
        Some("iterations") => {
            let raw = payload.iterations.as_deref().unwrap_or("");
            match raw.trim().parse::<u32>() {
                Ok(value) => (Some(value), None),
                Err(_) => {
                    errors.push(FieldError {
                        field: "iterations",
                        message: "iterations must be a positive number",
                        ui_message: "Enter the number of weeks",
                    });
                    (None, None)
                }
            }
        }
        Some("end_date") => {
            let raw = payload.end_date.as_deref().unwrap_or("");
            match NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d") {
                Ok(value) => (None, Some(value)),
                Err(_) => {
                    errors.push(FieldError {
                        field: "end_date",
                        message: "end date must be YYYY-MM-DD",
                        ui_message: "Pick an end date",
                    });
                    (None, None)
                }
            }
        }
        _ => (None, None),
    };
    let start_date = match payload.start_date.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => match NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            Ok(value) => Some(value),
            Err(_) => {
                errors.push(FieldError {
                    field: "start_date",
                    message: "start date must be YYYY-MM-DD",
                    ui_message: "Pick a start date",
                });
                None
            }
        },
    };
    let spec = build_spec(
        RawRoutine {
            title: payload.title.clone(),
            notes: payload.notes.clone(),
            kind: payload.kind.clone(),
            weekdays,
            per_period,
            period,
            iterations,
            end_date,
            start_date,
            time_of_day: payload.time_of_day.clone(),
            time_end: payload.time_end.clone(),
        },
        today,
    );
    match spec {
        Ok(value) => {
            if errors.is_empty() {
                return Ok(value);
            }
            return Err(errors);
        }
        Err(mut spec_errors) => {
            errors.append(&mut spec_errors);
            return Err(errors);
        }
    }
}

fn json_raw(payload: &RoutineJsonPayload) -> Result<RawRoutine, WebError> {
    let mut weekdays = Vec::new();
    for bit in payload.weekdays.iter().flatten() {
        match weekday_from_index(*bit) {
            Some(day) => weekdays.push(day),
            None => return Err(WebError::Validation("weekdays must be 0-6")),
        }
    }
    let period = match payload.period.as_deref() {
        None | Some("") => None,
        Some(raw) => {
            Some(store::parse_period(raw.trim()).ok_or(WebError::Validation("unknown period"))?)
        }
    };
    return Ok(RawRoutine {
        title: payload.title.clone(),
        notes: payload.notes.clone(),
        kind: payload.kind.clone(),
        weekdays,
        per_period: payload.per_period,
        period,
        iterations: payload.iterations,
        end_date: payload.end_date,
        start_date: payload.start_date,
        time_of_day: payload.time_of_day.clone(),
        time_end: payload.time_end.clone(),
    });
}

fn weekday_from_index(index: u8) -> Option<Weekday> {
    return match index {
        0 => Some(Weekday::Mon),
        1 => Some(Weekday::Tue),
        2 => Some(Weekday::Wed),
        3 => Some(Weekday::Thu),
        4 => Some(Weekday::Fri),
        5 => Some(Weekday::Sat),
        6 => Some(Weekday::Sun),
        _ => None,
    };
}

pub(crate) struct RawRoutine {
    pub title: Option<String>,
    pub notes: Option<String>,
    pub kind: Option<String>,
    pub weekdays: Vec<Weekday>,
    pub per_period: Option<u32>,
    pub period: Option<store::PeriodKind>,
    pub iterations: Option<u32>,
    pub end_date: Option<NaiveDate>,
    pub start_date: Option<NaiveDate>,
    pub time_of_day: Option<String>,
    pub time_end: Option<String>,
}

pub(crate) fn build_spec(
    raw: RawRoutine,
    today: NaiveDate,
) -> Result<store::RoutineSpec, Vec<FieldError>> {
    let mut errors = Vec::new();
    let title = match raw.title.as_deref() {
        Some(raw_title) => {
            let trimmed = raw_title.trim();
            if trimmed.is_empty() {
                errors.push(FieldError {
                    field: "title",
                    message: "title is required",
                    ui_message: "Enter a title",
                });
                String::new()
            } else if trimmed.chars().count() > 200 {
                errors.push(FieldError {
                    field: "title",
                    message: "title must be 1-200 characters",
                    ui_message: "Title must be 200 characters or fewer",
                });
                trimmed.to_string()
            } else {
                trimmed.to_string()
            }
        }
        None => {
            errors.push(FieldError {
                field: "title",
                message: "title is required",
                ui_message: "Enter a title",
            });
            String::new()
        }
    };
    let schedule = match raw.kind.as_deref() {
        Some("weekdays") => {
            let mask = mask_from_weekdays(&raw.weekdays);
            if mask == 0 {
                errors.push(FieldError {
                    field: "weekdays",
                    message: "select at least one weekday",
                    ui_message: "Pick at least one day",
                });
            }
            Some((store::ScheduleKind::Weekdays, Some(mask), None, None))
        }
        Some("count") => {
            let per_period = match raw.per_period {
                None => {
                    errors.push(FieldError {
                        field: "per_period",
                        message: "per_period is required",
                        ui_message: "Enter how many times",
                    });
                    None
                }
                Some(0) => {
                    errors.push(FieldError {
                        field: "per_period",
                        message: "per_period must be at least 1",
                        ui_message: "Times per period must be at least 1",
                    });
                    None
                }
                Some(value) => Some(value),
            };
            let period = match raw.period {
                None => {
                    errors.push(FieldError {
                        field: "period",
                        message: "period is required",
                        ui_message: "Choose a period",
                    });
                    None
                }
                Some(value) => Some(value),
            };
            Some((store::ScheduleKind::Count, None, period, per_period))
        }
        Some(_) => {
            errors.push(FieldError {
                field: "kind",
                message: "unknown kind",
                ui_message: "Choose a schedule type",
            });
            None
        }
        None => {
            errors.push(FieldError {
                field: "kind",
                message: "kind is required",
                ui_message: "Choose a schedule type",
            });
            None
        }
    };
    let start_date = raw.start_date.unwrap_or(today);
    if start_date < today {
        errors.push(FieldError {
            field: "start_date",
            message: "start date cannot be before today",
            ui_message: "Start date cannot be in the past",
        });
    }
    let (iterations, end_date) = match (raw.iterations, raw.end_date) {
        (Some(_), Some(_)) => {
            errors.push(FieldError {
                field: "iterations",
                message: "iterations and end_date are mutually exclusive",
                ui_message: "Choose either a number of weeks or an end date",
            });
            (None, None)
        }
        (Some(0), None) => {
            errors.push(FieldError {
                field: "iterations",
                message: "iterations must be at least 1",
                ui_message: "Weeks must be at least 1",
            });
            (None, None)
        }
        (Some(value), None) => (Some(value), None),
        (None, Some(value)) => {
            if value < today {
                errors.push(FieldError {
                    field: "end_date",
                    message: "end date cannot be before today",
                    ui_message: "End date cannot be before today",
                });
            } else if value < start_date {
                errors.push(FieldError {
                    field: "end_date",
                    message: "end date cannot be before start date",
                    ui_message: "End date cannot be before the start date",
                });
            }
            (None, Some(value))
        }
        (None, None) => (None, None),
    };
    let time_of_day = match validate_time(raw.time_of_day, "time_of_day") {
        Ok(value) => value,
        Err(error) => {
            errors.push(error);
            None
        }
    };
    let time_end = match validate_time(raw.time_end, "time_end") {
        Ok(value) => value,
        Err(error) => {
            errors.push(error);
            None
        }
    };
    if time_end.is_some() && time_of_day.is_none() {
        errors.push(FieldError {
            field: "time_end",
            message: "time end requires time of day",
            ui_message: "End time needs a start time",
        });
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let Some((kind, weekdays_mask, period, per_period)) = schedule else {
        return Err(errors);
    };
    return Ok(store::RoutineSpec {
        title,
        notes: raw.notes,
        kind,
        weekdays_mask,
        period,
        per_period,
        iterations,
        end_date,
        time_of_day: time_of_day.map(|(value, _)| value),
        time_end: time_end.map(|(value, _)| value),
        start_date: start_date.max(today),
    });
}

fn validate_time(
    raw: Option<String>,
    field: &'static str,
) -> Result<Option<(String, NaiveTime)>, FieldError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let Ok(value) = NaiveTime::parse_from_str(trimmed, "%H:%M") else {
        return Err(FieldError {
            field,
            message: "invalid time",
            ui_message: "Use HH:MM",
        });
    };
    return Ok(Some((value.format("%H:%M").to_string(), value)));
}
