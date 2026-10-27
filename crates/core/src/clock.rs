//! "Now", injectable. Everything that cares what day it is asks a `Clock`, so
//! tests can freeze time or jump to tomorrow.

use std::sync::Mutex;

use chrono::{DateTime, Duration, NaiveDate, Utc};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;

    /// The game day. Days reset at midnight UTC.
    fn today(&self) -> NaiveDate {
        self.now().date_naive()
    }
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock that only moves when told to.
pub struct FixedClock(Mutex<DateTime<Utc>>);

impl FixedClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self(Mutex::new(now))
    }

    /// Noon UTC on `day`.
    pub fn at_day(day: NaiveDate) -> Self {
        Self::new(day.and_hms_opt(12, 0, 0).expect("valid time").and_utc())
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self.0.lock().expect("clock lock") = now;
    }

    pub fn advance(&self, by: Duration) {
        *self.0.lock().expect("clock lock") += by;
    }
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().expect("clock lock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_clock_moves_only_when_told() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        let clock = FixedClock::at_day(day);
        assert_eq!(clock.today(), day);
        clock.advance(Duration::hours(12));
        assert_eq!(clock.today(), day.succ_opt().unwrap());
        clock.set(day.and_hms_opt(0, 0, 0).unwrap().and_utc());
        assert_eq!(clock.today(), day);
    }

    #[test]
    fn the_system_clock_is_close_to_now() {
        assert!((SystemClock.now() - Utc::now()).num_seconds().abs() < 5);
    }
}
