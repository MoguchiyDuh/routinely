use chrono::Datelike;
use chrono::NaiveDate;
use chrono::Weekday;

use routinely::recurrence::PeriodKind;
use routinely::recurrence::Schedule;
use routinely::recurrence::ScheduleKind;
use routinely::recurrence::effective_end;
use routinely::recurrence::is_finished_on;
use routinely::recurrence::mask_contains;
use routinely::recurrence::mask_from_weekdays;
use routinely::recurrence::misses_for_period;
use routinely::recurrence::occurrences_in_range;
use routinely::recurrence::period_bounds;
use routinely::recurrence::periods_in_range;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    return NaiveDate::from_ymd_opt(year, month, day).unwrap();
}

fn weekdays_schedule(
    mask: u8,
    iterations: Option<u32>,
    start: NaiveDate,
    end: Option<NaiveDate>,
) -> Schedule {
    return Schedule {
        kind: ScheduleKind::Weekdays,
        weekdays_mask: mask,
        period: PeriodKind::Day,
        per_period: 1,
        iterations,
        start_date: start,
        end_date: end,
    };
}

fn count_schedule(
    period: PeriodKind,
    per_period: u32,
    iterations: Option<u32>,
    start: NaiveDate,
    end: Option<NaiveDate>,
) -> Schedule {
    return Schedule {
        kind: ScheduleKind::Count,
        weekdays_mask: 0,
        period,
        per_period,
        iterations,
        start_date: start,
        end_date: end,
    };
}

#[test]
fn gym_mon_wed_fri_infinite_due_dates() {
    let mask = mask_from_weekdays(&[Weekday::Mon, Weekday::Wed, Weekday::Fri]);
    let schedule = weekdays_schedule(mask, None, date(2026, 9, 1), None);
    let occurrences = occurrences_in_range(&schedule, date(2026, 9, 1), date(2026, 9, 30));
    assert_eq!(occurrences.len(), 13);
    assert_eq!(occurrences[0], date(2026, 9, 2));
    assert_eq!(occurrences[1], date(2026, 9, 4));
    assert_eq!(occurrences[12], date(2026, 9, 30));
    for day in &occurrences {
        assert!(matches!(
            day.weekday(),
            Weekday::Mon | Weekday::Wed | Weekday::Fri
        ));
    }
}

#[test]
fn wake_up_daily_one_slot_per_day() {
    let schedule = count_schedule(PeriodKind::Day, 1, None, date(2026, 9, 1), None);
    let periods = periods_in_range(&schedule, date(2026, 9, 1), date(2026, 9, 7));
    assert_eq!(periods.len(), 7);
    for (index, period) in periods.iter().enumerate() {
        assert_eq!(period.period_start, period.period_end);
        assert_eq!(period.required, 1);
        assert_eq!(period.period_index, index as u32);
    }
    assert_eq!(periods[0].period_start, date(2026, 9, 1));
    assert_eq!(periods[6].period_start, date(2026, 9, 7));
}

#[test]
fn learn_c_daily_one_month_terminates() {
    let schedule = count_schedule(PeriodKind::Day, 1, Some(30), date(2026, 9, 1), None);
    let periods = periods_in_range(&schedule, date(2026, 9, 1), date(2026, 12, 31));
    assert_eq!(periods.len(), 30);
    assert_eq!(periods[29].period_start, date(2026, 9, 30));
    assert_eq!(periods[29].period_end, date(2026, 9, 30));
    assert_eq!(periods[29].period_index, 29);
    assert!(!is_finished_on(&schedule, date(2026, 9, 29)));
    assert!(is_finished_on(&schedule, date(2026, 9, 30)));
    assert!(is_finished_on(&schedule, date(2027, 1, 1)));
    assert!(occurrences_in_range(&schedule, date(2026, 9, 1), date(2026, 9, 30)).is_empty());
}

#[test]
fn groceries_twice_per_week_infinite() {
    let schedule = count_schedule(PeriodKind::Week, 2, None, date(2026, 9, 21), None);
    let periods = periods_in_range(&schedule, date(2026, 9, 21), date(2026, 10, 4));
    assert_eq!(periods.len(), 2);
    assert_eq!(periods[0].period_start, date(2026, 9, 21));
    assert_eq!(periods[0].period_end, date(2026, 9, 27));
    assert_eq!(periods[0].required, 2);
    assert_eq!(periods[0].period_index, 0);
    assert_eq!(periods[1].period_start, date(2026, 9, 28));
    assert_eq!(periods[1].period_end, date(2026, 10, 4));
    assert_eq!(periods[1].required, 2);
    assert_eq!(periods[1].period_index, 1);

    let midweek = count_schedule(PeriodKind::Week, 2, None, date(2026, 9, 23), None);
    let periods = periods_in_range(&midweek, date(2026, 9, 21), date(2026, 10, 4));
    assert_eq!(periods.len(), 2);
    assert_eq!(periods[0].period_start, date(2026, 9, 21));
    assert_eq!(periods[0].period_index, 0);
    assert_eq!(periods[1].period_index, 1);
}

#[test]
fn hospital_once_per_week_three_weeks_then_finished() {
    let schedule = count_schedule(PeriodKind::Week, 1, Some(3), date(2026, 9, 21), None);
    let periods = periods_in_range(&schedule, date(2026, 9, 21), date(2026, 12, 31));
    assert_eq!(periods.len(), 3);
    assert_eq!(periods[2].period_start, date(2026, 10, 5));
    assert_eq!(periods[2].period_end, date(2026, 10, 11));
    assert_eq!(periods[2].period_index, 2);
    assert!(!is_finished_on(&schedule, date(2026, 9, 27)));
    assert!(!is_finished_on(&schedule, date(2026, 10, 4)));
    assert!(!is_finished_on(&schedule, date(2026, 10, 10)));
    assert!(is_finished_on(&schedule, date(2026, 10, 11)));
    assert!(is_finished_on(&schedule, date(2026, 12, 25)));
}

#[test]
fn mask_bits_monday_is_bit_zero_and_round_trip() {
    assert_eq!(mask_from_weekdays(&[Weekday::Mon]), 1);
    assert_eq!(mask_from_weekdays(&[Weekday::Tue]), 2);
    assert_eq!(mask_from_weekdays(&[Weekday::Sun]), 64);
    assert_eq!(
        mask_from_weekdays(&[Weekday::Mon, Weekday::Wed, Weekday::Fri]),
        21
    );
    let days = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];
    let all = mask_from_weekdays(&days);
    assert_eq!(all, 127);
    assert!(!mask_contains(0, Weekday::Mon));
    for day in days {
        assert!(mask_contains(all, day));
        let single = mask_from_weekdays(&[day]);
        for other in days {
            assert_eq!(mask_contains(single, other), day == other);
        }
    }
}

#[test]
fn period_bounds_iso_week_crosses_year_boundary() {
    assert_eq!(
        period_bounds(date(2026, 1, 1), PeriodKind::Week).unwrap(),
        (date(2025, 12, 29), date(2026, 1, 4))
    );
    assert_eq!(
        period_bounds(date(2025, 1, 1), PeriodKind::Week).unwrap(),
        (date(2024, 12, 30), date(2025, 1, 5))
    );
    assert_eq!(
        period_bounds(date(2025, 12, 31), PeriodKind::Week).unwrap(),
        (date(2025, 12, 29), date(2026, 1, 4))
    );
    assert_eq!(
        period_bounds(date(2026, 9, 23), PeriodKind::Week).unwrap(),
        (date(2026, 9, 21), date(2026, 9, 27))
    );
}

#[test]
fn period_bounds_month_feb_dec_jan_year_day() {
    assert_eq!(
        period_bounds(date(2024, 2, 10), PeriodKind::Month).unwrap(),
        (date(2024, 2, 1), date(2024, 2, 29))
    );
    assert_eq!(
        period_bounds(date(2026, 2, 10), PeriodKind::Month).unwrap(),
        (date(2026, 2, 1), date(2026, 2, 28))
    );
    assert_eq!(
        period_bounds(date(2025, 12, 15), PeriodKind::Month).unwrap(),
        (date(2025, 12, 1), date(2025, 12, 31))
    );
    assert_eq!(
        period_bounds(date(2026, 1, 15), PeriodKind::Month).unwrap(),
        (date(2026, 1, 1), date(2026, 1, 31))
    );
    assert_eq!(
        period_bounds(date(2026, 2, 10), PeriodKind::Year).unwrap(),
        (date(2026, 1, 1), date(2026, 12, 31))
    );
    assert_eq!(
        period_bounds(date(2026, 9, 23), PeriodKind::Day).unwrap(),
        (date(2026, 9, 23), date(2026, 9, 23))
    );
}

#[test]
fn effective_end_end_date_iterations_and_infinite() {
    let start = date(2026, 9, 21);
    let end_only = weekdays_schedule(127, None, start, Some(date(2026, 10, 15)));
    assert_eq!(effective_end(&end_only), Some(date(2026, 10, 15)));

    let iterations_only = weekdays_schedule(127, Some(2), start, None);
    assert_eq!(effective_end(&iterations_only), Some(date(2026, 10, 4)));

    let both = weekdays_schedule(127, Some(2), start, Some(date(2026, 10, 15)));
    assert_eq!(effective_end(&both), Some(date(2026, 10, 4)));

    let both_other_way = weekdays_schedule(127, Some(4), start, Some(date(2026, 10, 15)));
    assert_eq!(effective_end(&both_other_way), Some(date(2026, 10, 15)));

    let infinite = weekdays_schedule(127, None, start, None);
    assert_eq!(effective_end(&infinite), None);

    let midweek = weekdays_schedule(127, Some(1), date(2026, 9, 23), None);
    assert_eq!(effective_end(&midweek), Some(date(2026, 9, 27)));
}

#[test]
fn occurrences_in_range_clamps_to_active_window() {
    let start = date(2026, 9, 21);
    let mask = mask_from_weekdays(&[Weekday::Mon, Weekday::Wed, Weekday::Fri]);

    let infinite = weekdays_schedule(mask, None, start, None);
    assert!(occurrences_in_range(&infinite, date(2026, 9, 1), date(2026, 9, 20)).is_empty());

    let ended = weekdays_schedule(mask, None, start, Some(date(2026, 9, 30)));
    assert!(occurrences_in_range(&ended, date(2026, 10, 1), date(2026, 10, 31)).is_empty());

    let straddling = weekdays_schedule(127, None, date(2026, 9, 23), None);
    let occurrences = occurrences_in_range(&straddling, date(2026, 9, 21), date(2026, 9, 27));
    assert_eq!(occurrences.len(), 5);
    assert_eq!(occurrences[0], date(2026, 9, 23));
    assert_eq!(occurrences[4], date(2026, 9, 27));

    let iter_clipped = weekdays_schedule(mask, Some(1), start, None);
    let occurrences = occurrences_in_range(&iter_clipped, date(2026, 9, 1), date(2026, 10, 31));
    assert_eq!(
        occurrences,
        vec![date(2026, 9, 21), date(2026, 9, 23), date(2026, 9, 25)]
    );
}

#[test]
fn periods_in_range_month_iterations_and_straddling_edges() {
    let schedule = count_schedule(PeriodKind::Month, 4, Some(2), date(2026, 1, 15), None);
    let periods = periods_in_range(&schedule, date(2026, 1, 1), date(2026, 12, 31));
    assert_eq!(periods.len(), 2);
    assert_eq!(periods[0].period_start, date(2026, 1, 1));
    assert_eq!(periods[0].period_end, date(2026, 1, 31));
    assert_eq!(periods[0].period_index, 0);
    assert_eq!(periods[1].period_start, date(2026, 2, 1));
    assert_eq!(periods[1].period_end, date(2026, 2, 28));
    assert_eq!(periods[1].period_index, 1);

    let open = count_schedule(PeriodKind::Month, 1, None, date(2026, 1, 1), None);
    let periods = periods_in_range(&open, date(2026, 1, 20), date(2026, 2, 10));
    assert_eq!(periods.len(), 2);
    assert_eq!(periods[0].period_start, date(2026, 1, 1));
    assert_eq!(periods[0].period_end, date(2026, 1, 31));
    assert_eq!(periods[1].period_start, date(2026, 2, 1));
    assert_eq!(periods[1].period_end, date(2026, 2, 28));

    assert!(periods_in_range(&open, date(2025, 1, 1), date(2025, 6, 1)).is_empty());
    assert!(periods_in_range(&open, date(2026, 3, 1), date(2026, 1, 1)).is_empty());
}

#[test]
fn periods_in_range_crosses_december_into_january() {
    let schedule = count_schedule(PeriodKind::Week, 2, None, date(2025, 12, 29), None);
    let periods = periods_in_range(&schedule, date(2025, 12, 1), date(2026, 1, 31));
    assert_eq!(periods.len(), 5);
    assert_eq!(periods[0].period_start, date(2025, 12, 29));
    assert_eq!(periods[0].period_end, date(2026, 1, 4));
    assert_eq!(periods[4].period_start, date(2026, 1, 26));
    assert_eq!(periods[4].period_end, date(2026, 2, 1));
    for (index, period) in periods.iter().enumerate() {
        assert_eq!(period.period_index, index as u32);
    }
}

#[test]
fn misses_for_period_exactly_one_when_short() {
    assert_eq!(misses_for_period(2, 2), 0);
    assert_eq!(misses_for_period(2, 5), 0);
    assert_eq!(misses_for_period(1, 0), 1);
    assert_eq!(misses_for_period(2, 1), 1);
    assert_eq!(misses_for_period(5, 3), 1);
    assert_eq!(misses_for_period(5, 4), 1);
}

#[test]
fn is_finished_on_both_kinds() {
    let infinite = weekdays_schedule(127, None, date(2026, 9, 21), None);
    assert!(!is_finished_on(&infinite, date(2026, 9, 20)));
    assert!(!is_finished_on(&infinite, date(3000, 1, 1)));

    let ended = weekdays_schedule(127, None, date(2026, 9, 21), Some(date(2026, 9, 30)));
    assert!(!is_finished_on(&ended, date(2026, 9, 20)));
    assert!(!is_finished_on(&ended, date(2026, 9, 30)));
    assert!(is_finished_on(&ended, date(2026, 10, 1)));

    let bounded = count_schedule(PeriodKind::Week, 1, Some(3), date(2026, 9, 21), None);
    assert!(!is_finished_on(&bounded, date(2026, 10, 4)));
    assert!(is_finished_on(&bounded, date(2026, 10, 11)));

    let count_ended = count_schedule(
        PeriodKind::Day,
        1,
        None,
        date(2026, 9, 21),
        Some(date(2026, 9, 25)),
    );
    assert!(!is_finished_on(&count_ended, date(2026, 9, 24)));
    assert!(is_finished_on(&count_ended, date(2026, 9, 25)));
    assert!(is_finished_on(&count_ended, date(2026, 9, 26)));

    let count_open = count_schedule(PeriodKind::Day, 1, None, date(2026, 9, 21), None);
    assert!(!is_finished_on(&count_open, date(3000, 1, 1)));
}
