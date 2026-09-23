use chrono::Datelike;
use chrono::Days;
use chrono::NaiveDate;
use chrono::Weekday;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleKind {
    Weekdays,
    Count,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodKind {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub kind: ScheduleKind,
    pub weekdays_mask: u8,
    pub period: PeriodKind,
    pub per_period: u32,
    pub iterations: Option<u32>,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub required: u32,
    pub period_index: u32,
}

pub fn mask_from_weekdays(weekdays: &[Weekday]) -> u8 {
    let mut mask = 0u8;
    for weekday in weekdays {
        mask |= 1u8 << weekday.num_days_from_monday();
    }
    return mask;
}

pub fn mask_contains(mask: u8, weekday: Weekday) -> bool {
    return mask & (1u8 << weekday.num_days_from_monday()) != 0;
}

pub fn period_bounds(date: NaiveDate, period: PeriodKind) -> Option<(NaiveDate, NaiveDate)> {
    return match period {
        PeriodKind::Day => Some((date, date)),
        PeriodKind::Week => {
            let offset = u64::from(date.weekday().num_days_from_monday());
            let start = date.checked_sub_days(Days::new(offset))?;
            let end = start.checked_add_days(Days::new(6))?;
            return Some((start, end));
        }
        PeriodKind::Month => {
            let year = date.year();
            let month = date.month();
            let start = NaiveDate::from_ymd_opt(year, month, 1)?;
            let (next_year, next_month) = if month == 12 {
                (year.checked_add(1)?, 1)
            } else {
                (year, month + 1)
            };
            let next_start = NaiveDate::from_ymd_opt(next_year, next_month, 1)?;
            let end = next_start.pred_opt()?;
            return Some((start, end));
        }
        PeriodKind::Year => {
            let start = NaiveDate::from_ymd_opt(date.year(), 1, 1)?;
            let end = NaiveDate::from_ymd_opt(date.year(), 12, 31)?;
            return Some((start, end));
        }
    };
}

pub fn effective_end(schedule: &Schedule) -> Option<NaiveDate> {
    let iterations_end = match schedule.iterations {
        Some(weeks) => iterations_window_end(schedule.start_date, weeks),
        None => None,
    };
    return match (schedule.end_date, iterations_end) {
        (Some(end_date), Some(iter_end)) => Some(end_date.min(iter_end)),
        (Some(end_date), None) => Some(end_date),
        (None, Some(iter_end)) => Some(iter_end),
        (None, None) => None,
    };
}

fn iterations_window_end(start_date: NaiveDate, weeks: u32) -> Option<NaiveDate> {
    let span = u64::from(weeks).checked_mul(7)?.checked_sub(1)?;
    let offset = u64::from(start_date.weekday().num_days_from_monday());
    let week_start = start_date
        .checked_sub_days(Days::new(offset))
        .unwrap_or(NaiveDate::MIN);
    let end = match week_start.checked_add_days(Days::new(span)) {
        Some(date) => date,
        None => return Some(NaiveDate::MAX),
    };
    return Some(end);
}

pub fn occurrences_in_range(
    schedule: &Schedule,
    range_start: NaiveDate,
    range_end: NaiveDate,
) -> Vec<NaiveDate> {
    if schedule.kind != ScheduleKind::Weekdays {
        return Vec::new();
    }
    let lo = schedule.start_date.max(range_start);
    let hi = match effective_end(schedule) {
        Some(end) => end.min(range_end),
        None => range_end,
    };
    if lo > hi {
        return Vec::new();
    }
    let mut dates = Vec::new();
    let mut current = lo;
    loop {
        if mask_contains(schedule.weekdays_mask, current.weekday()) {
            dates.push(current);
        }
        if current >= hi {
            break;
        }
        match current.checked_add_days(Days::new(1)) {
            Some(next) => current = next,
            None => break,
        }
    }
    return dates;
}

pub fn periods_in_range(
    schedule: &Schedule,
    range_start: NaiveDate,
    range_end: NaiveDate,
) -> Vec<Period> {
    if schedule.kind != ScheduleKind::Count {
        return Vec::new();
    }
    let lo = schedule.start_date.max(range_start);
    let hi = match schedule.end_date {
        Some(end_date) => end_date.min(range_end),
        None => range_end,
    };
    if lo > hi {
        return Vec::new();
    }
    let mut current = match period_bounds(lo, schedule.period) {
        Some((start, _)) => start,
        None => return Vec::new(),
    };
    let mut periods = Vec::new();
    while let Some((start, end)) = period_bounds(current, schedule.period) {
        if start > hi {
            break;
        }
        let index = period_index(schedule, start);
        if schedule.iterations.is_some_and(|weeks| index >= weeks) {
            break;
        }
        periods.push(Period {
            period_start: start,
            period_end: end,
            required: schedule.per_period,
            period_index: index,
        });
        match next_period_start(start, schedule.period) {
            Some(next) => current = next,
            None => break,
        }
    }
    return periods;
}

pub fn misses_for_period(required: u32, completions: u32) -> u32 {
    if completions >= required {
        return 0;
    }
    return 1;
}

pub fn is_finished_on(schedule: &Schedule, date: NaiveDate) -> bool {
    return match schedule.kind {
        ScheduleKind::Weekdays => match effective_end(schedule) {
            Some(end) => date > end,
            None => false,
        },
        ScheduleKind::Count => match last_count_period(schedule) {
            Some(period) => date >= period.period_end,
            None => false,
        },
    };
}

fn last_count_period(schedule: &Schedule) -> Option<Period> {
    if schedule.iterations.is_none() && schedule.end_date.is_none() {
        return None;
    }
    let hi = schedule.end_date.unwrap_or(NaiveDate::MAX);
    let mut periods = periods_in_range(schedule, schedule.start_date, hi);
    return periods.pop();
}

fn next_period_start(current: NaiveDate, period: PeriodKind) -> Option<NaiveDate> {
    return match period {
        PeriodKind::Day => current.checked_add_days(Days::new(1)),
        PeriodKind::Week => current.checked_add_days(Days::new(7)),
        PeriodKind::Month => {
            let (year, month) = if current.month() == 12 {
                (current.year().checked_add(1)?, 1)
            } else {
                (current.year(), current.month() + 1)
            };
            return NaiveDate::from_ymd_opt(year, month, 1);
        }
        PeriodKind::Year => NaiveDate::from_ymd_opt(current.year().checked_add(1)?, 1, 1),
    };
}

fn period_index(schedule: &Schedule, period_start: NaiveDate) -> u32 {
    match schedule.period {
        PeriodKind::Day => {
            let days = (period_start - schedule.start_date).num_days();
            return u32::try_from(days).unwrap_or(0);
        }
        PeriodKind::Week => {
            let anchor = match period_bounds(schedule.start_date, PeriodKind::Week) {
                Some((start, _)) => start,
                None => return 0,
            };
            let weeks = (period_start - anchor).num_days() / 7;
            return u32::try_from(weeks).unwrap_or(0);
        }
        PeriodKind::Month => {
            let (anchor_year, anchor_month) =
                match period_bounds(schedule.start_date, PeriodKind::Month) {
                    Some((start, _)) => (start.year(), start.month()),
                    None => return 0,
                };
            let months = i64::from(period_start.year() - anchor_year) * 12
                + i64::from(period_start.month())
                - i64::from(anchor_month);
            return u32::try_from(months).unwrap_or(0);
        }
        PeriodKind::Year => {
            let years = period_start.year() - schedule.start_date.year();
            return u32::try_from(years).unwrap_or(0);
        }
    };
}
