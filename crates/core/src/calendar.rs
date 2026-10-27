//! Day numbers, streaks and the daily reset. The game day is the UTC date,
//! so everyone everywhere plays the same set and it resets at midnight UTC.

use std::collections::BTreeSet;

use chrono::{DateTime, Days, NaiveDate, Utc};

/// The day's number counted from launch, which is day 1. None before launch.
pub fn day_number(launch: NaiveDate, day: NaiveDate) -> Option<u32> {
    let days = (day - launch).num_days();
    u32::try_from(days).ok().map(|n| n + 1)
}

/// The game day an instant falls on.
pub fn game_day(now: DateTime<Utc>) -> NaiveDate {
    now.date_naive()
}

/// When the next day's set goes up.
pub fn next_reset(now: DateTime<Utc>) -> DateTime<Utc> {
    (game_day(now) + Days::new(1))
        .and_hms_opt(0, 0, 0)
        .expect("midnight exists")
        .and_utc()
}

/// Consecutive completed days up to today. A streak still counts while
/// today isn't finished yet: it only breaks once a whole day is missed.
/// Days listed twice count once; days after `today` are ignored.
pub fn streak(completed: &[NaiveDate], today: NaiveDate) -> u32 {
    let days: BTreeSet<NaiveDate> = completed.iter().copied().collect();
    let mut day = if days.contains(&today) {
        today
    } else {
        match today.pred_opt() {
            Some(yesterday) => yesterday,
            None => return 0,
        }
    };
    let mut count = 0;
    while days.contains(&day) {
        count += 1;
        match day.pred_opt() {
            Some(before) => day = before,
            None => break,
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> NaiveDate {
        text.parse().unwrap()
    }

    #[test]
    fn launch_day_is_day_one() {
        let launch = d("2026-10-27");
        assert_eq!(day_number(launch, launch), Some(1));
        assert_eq!(day_number(launch, d("2027-10-27")), Some(366));
        assert_eq!(day_number(launch, d("2026-10-26")), None);
    }

    #[test]
    fn a_streak_survives_until_a_whole_day_is_missed() {
        let today = d("2026-10-27");
        let days = [d("2026-10-25"), d("2026-10-26")];
        assert_eq!(streak(&days, today), 2, "today isn't over yet");
        assert_eq!(streak(&[days[0], days[1], today], today), 3);
        assert_eq!(streak(&[d("2026-10-25")], today), 0);
        assert_eq!(streak(&[], today), 0);
        assert_eq!(
            streak(&[d("2026-10-28")], today),
            0,
            "the future doesn't count"
        );
    }

    #[test]
    fn the_reset_is_the_next_utc_midnight() {
        let at = |text: &str| text.parse::<DateTime<Utc>>().unwrap();
        assert_eq!(
            next_reset(at("2026-10-27T23:59:59Z")),
            at("2026-10-28T00:00:00Z")
        );
        assert_eq!(
            next_reset(at("2026-10-28T00:00:00Z")),
            at("2026-10-29T00:00:00Z")
        );
        // 17:30 in Los Angeles is already tomorrow's game.
        let la = "2026-10-27T17:30:00-07:00".parse::<DateTime<chrono::FixedOffset>>();
        assert_eq!(game_day(la.unwrap().with_timezone(&Utc)), d("2026-10-28"));
    }
}

#[cfg(test)]
mod properties {
    use chrono::{Duration, FixedOffset, TimeZone};
    use proptest::prelude::*;

    use super::*;

    fn date() -> impl Strategy<Value = NaiveDate> {
        (0i64..20_000).prop_map(|n| d0() + Days::new(n as u64))
    }

    fn d0() -> NaiveDate {
        NaiveDate::from_ymd_opt(2000, 1, 1).unwrap()
    }

    proptest! {
        #[test]
        fn day_numbers_count_up_by_one(launch in date(), offset in 0u64..10_000) {
            let day = launch + Days::new(offset);
            prop_assert_eq!(day_number(launch, day), Some(offset as u32 + 1));
            prop_assert_eq!(day_number(launch, day + Days::new(1)), Some(offset as u32 + 2));
        }

        #[test]
        fn an_unbroken_run_is_its_length_and_repeats_add_nothing(
            today in date(), len in 0u64..60, repeats in 1usize..4,
        ) {
            let run: Vec<NaiveDate> = (0..len).map(|i| today - Days::new(i)).collect();
            prop_assert_eq!(streak(&run, today), len as u32);
            let repeated: Vec<NaiveDate> = run.iter().flat_map(|d| std::iter::repeat_n(*d, repeats)).collect();
            prop_assert_eq!(streak(&repeated, today), len as u32);
        }

        #[test]
        fn a_gap_breaks_the_streak(today in date(), run in 1u64..30, older in 0u64..30) {
            // `run` days ending today, then one missed day, then older days.
            let mut days: Vec<NaiveDate> = (0..run).map(|i| today - Days::new(i)).collect();
            days.extend((run + 1..run + 1 + older).map(|i| today - Days::new(i)));
            prop_assert_eq!(streak(&days, today), run as u32);
            // Missing yesterday as well as today ends it outright.
            prop_assert_eq!(streak(&days[1..], today + Days::new(1)), 0);
        }

        #[test]
        fn the_game_day_is_the_utc_date_wherever_the_player_is(
            secs in 0i64..2_000_000_000, offset_hours in -12i32..=14,
        ) {
            let instant = Utc.timestamp_opt(secs, 0).unwrap();
            let local = FixedOffset::east_opt(offset_hours * 3600).unwrap();
            let seen_from_anywhere = instant.with_timezone(&local).with_timezone(&Utc);
            prop_assert_eq!(game_day(seen_from_anywhere), instant.date_naive());
            let reset = next_reset(instant);
            prop_assert!(reset > instant && reset - instant <= Duration::days(1));
            prop_assert_eq!(game_day(reset), game_day(instant) + Days::new(1));
        }
    }
}
