//! A clock end-to-end tests can move from outside the process: it reads an
//! RFC 3339 timestamp from a file on every call, falling back to the real
//! time when the file is missing or unreadable. Only compiled with the
//! `test-clock` feature, so a production build can't contain it.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use dorman_core::Clock;

pub struct FileClock(PathBuf);

impl FileClock {
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }
}

impl Clock for FileClock {
    fn now(&self) -> DateTime<Utc> {
        std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|text| DateTime::parse_from_rfc3339(text.trim()).ok())
            .map_or_else(Utc::now, |time| time.with_timezone(&Utc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_file_and_falls_back_to_now() {
        let path = std::env::temp_dir().join(format!("dorman-clock-{}", std::process::id()));
        std::fs::write(&path, "2026-10-27T12:00:00Z\n").unwrap();
        let clock = FileClock::new(path.clone());
        assert_eq!(clock.today().to_string(), "2026-10-27");
        std::fs::remove_file(&path).unwrap();
        assert!((clock.now() - Utc::now()).num_seconds().abs() < 5);
    }
}
