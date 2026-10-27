//! Ranks and the weekly leaderboard. Both reward being right, never doing
//! more: a rank needs accuracy against settled answers plus enough days
//! played, and the leaderboard orders by accuracy alone once a player has
//! answered enough to be measured.

/// Rookie → Doorman → Bouncer → Head of Security.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    Rookie,
    Doorman,
    Bouncer,
    HeadOfSecurity,
}

impl Rank {
    pub fn name(self) -> &'static str {
        match self {
            Rank::Rookie => "Rookie",
            Rank::Doorman => "Doorman",
            Rank::Bouncer => "Bouncer",
            Rank::HeadOfSecurity => "Head of Security",
        }
    }
}

/// A player's track record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Record {
    /// Daily games completed.
    pub days: u32,
    /// Answers on items the crowd has since settled.
    pub scored: u32,
    /// How many of those matched the settled answer.
    pub right: u32,
}

impl Record {
    pub fn accuracy(&self) -> f64 {
        if self.scored == 0 {
            0.0
        } else {
            f64::from(self.right) / f64::from(self.scored)
        }
    }
}

/// Below this many settled answers, accuracy says too little to rank on.
pub const MIN_SCORED: u32 = 10;

/// (rank, days played, accuracy) needed, highest first.
const THRESHOLDS: [(Rank, u32, f64); 3] = [
    (Rank::HeadOfSecurity, 30, 0.85),
    (Rank::Bouncer, 14, 0.75),
    (Rank::Doorman, 3, 0.60),
];

pub fn rank(record: Record) -> Rank {
    if record.scored < MIN_SCORED {
        return Rank::Rookie;
    }
    THRESHOLDS
        .iter()
        .find(|(_, days, accuracy)| record.days >= *days && record.accuracy() >= *accuracy)
        .map_or(Rank::Rookie, |(rank, _, _)| *rank)
}

/// What a player still needs for the next rank up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NextRank {
    pub rank: Rank,
    /// More days to play (0 if they have enough).
    pub days_to_go: u32,
    /// The accuracy to reach, as a whole percent.
    pub accuracy_percent: u32,
    /// Whether their accuracy is already there (once enough answers have settled).
    pub accuracy_met: bool,
    /// More settled answers needed before accuracy can be judged at all.
    pub answers_to_go: u32,
}

/// The next rank up from `record`'s, or None for the top rank.
pub fn next_rank(record: Record) -> Option<NextRank> {
    let current = rank(record);
    let (target, days, accuracy) = THRESHOLDS
        .iter()
        .rev()
        .find(|(r, _, _)| *r > current)
        .copied()?;
    Some(NextRank {
        rank: target,
        days_to_go: days.saturating_sub(record.days),
        accuracy_percent: (accuracy * 100.0).round() as u32,
        accuracy_met: record.scored >= MIN_SCORED && record.accuracy() >= accuracy,
        answers_to_go: MIN_SCORED.saturating_sub(record.scored),
    })
}

/// One player's week, as the leaderboard sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Standing {
    pub handle: String,
    pub scored: u32,
    pub right: u32,
    /// Flagged by the scorer's behaviour checks.
    pub flagged: bool,
}

/// Settled answers a player needs in the week to appear on the board.
pub const MIN_WEEKLY_SCORED: u32 = 20;

/// The board: unflagged players with enough settled answers, by accuracy;
/// ties go to more settled answers, then to the handle, so the order is
/// stable whatever order the rows came in.
pub fn leaderboard(mut standings: Vec<Standing>) -> Vec<Standing> {
    standings.retain(|s| !s.flagged && s.scored >= MIN_WEEKLY_SCORED);
    standings.sort_by(|a, b| {
        // right/scored compared exactly, without floating point.
        let (left, right) = (
            u64::from(a.right) * u64::from(b.scored),
            u64::from(b.right) * u64::from(a.scored),
        );
        right
            .cmp(&left)
            .then(b.scored.cmp(&a.scored))
            .then_with(|| a.handle.cmp(&b.handle))
    });
    standings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(days: u32, scored: u32, right: u32) -> Record {
        Record {
            days,
            scored,
            right,
        }
    }

    #[test]
    fn the_next_rank_says_what_is_missing() {
        let next = next_rank(record(1, 4, 4)).unwrap();
        assert_eq!(next.rank, Rank::Doorman);
        assert_eq!((next.days_to_go, next.answers_to_go), (2, 6));
        assert_eq!(next.accuracy_percent, 60);

        let next = next_rank(record(3, 20, 12)).unwrap();
        assert_eq!(next.rank, Rank::Bouncer, "a Doorman looks to Bouncer");
        assert_eq!((next.days_to_go, next.answers_to_go), (11, 0));
        assert!(!next.accuracy_met, "60% is short of 75%");

        let next = next_rank(record(30, 20, 15)).unwrap();
        assert_eq!(next.rank, Rank::HeadOfSecurity);
        assert!(!next.accuracy_met);
        assert_eq!(next_rank(record(30, 20, 18)), None, "nothing above the top");
    }

    #[test]
    fn ranks_change_exactly_at_their_thresholds() {
        assert_eq!(rank(record(3, 10, 6)), Rank::Doorman);
        assert_eq!(rank(record(2, 10, 6)), Rank::Rookie, "a day short");
        assert_eq!(rank(record(3, 10, 5)), Rank::Rookie, "59% isn't 60%");
        assert_eq!(
            rank(record(3, 9, 9)),
            Rank::Rookie,
            "too few settled answers"
        );
        assert_eq!(rank(record(14, 20, 15)), Rank::Bouncer);
        assert_eq!(rank(record(13, 20, 15)), Rank::Doorman);
        assert_eq!(rank(record(14, 100, 74)), Rank::Doorman);
        assert_eq!(rank(record(30, 20, 17)), Rank::HeadOfSecurity);
        assert_eq!(rank(record(29, 20, 17)), Rank::Bouncer);
        assert_eq!(rank(record(30, 100, 84)), Rank::Bouncer);
        assert_eq!(rank(Record::default()), Rank::Rookie);
        assert_eq!(Rank::HeadOfSecurity.name(), "Head of Security");
    }

    fn standing(handle: &str, scored: u32, right: u32) -> Standing {
        Standing {
            handle: handle.into(),
            scored,
            right,
            flagged: false,
        }
    }

    #[test]
    fn the_board_leaves_out_the_flagged_and_the_barely_played() {
        let mut flagged = standing("troll", 50, 50);
        flagged.flagged = true;
        let board = leaderboard(vec![
            standing("few", MIN_WEEKLY_SCORED - 1, MIN_WEEKLY_SCORED - 1),
            flagged,
            standing("ok", MIN_WEEKLY_SCORED, 10),
        ]);
        assert_eq!(board, vec![standing("ok", MIN_WEEKLY_SCORED, 10)]);
    }

    #[test]
    fn ties_go_to_more_answers_then_the_handle() {
        let board = leaderboard(vec![
            standing("b", 20, 15),
            standing("a", 20, 15),
            standing("c", 40, 30),
            standing("d", 20, 19),
        ]);
        let order: Vec<&str> = board.iter().map(|s| s.handle.as_str()).collect();
        assert_eq!(order, ["d", "c", "a", "b"]);
    }
}

#[cfg(test)]
mod properties {
    use proptest::prelude::*;

    use super::*;

    fn record() -> impl Strategy<Value = Record> {
        (0u32..60, 0u32..200)
            .prop_flat_map(|(days, scored)| (Just(days), Just(scored), 0..=scored))
            .prop_map(|(days, scored, right)| Record {
                days,
                scored,
                right,
            })
    }

    proptest! {
        #[test]
        fn more_days_or_more_accuracy_never_lowers_a_rank(r in record(), extra_days in 0u32..30) {
            let more_days = Record { days: r.days + extra_days, ..r };
            prop_assert!(rank(more_days) >= rank(r));
            if r.right < r.scored {
                let one_more_right = Record { right: r.right + 1, ..r };
                prop_assert!(rank(one_more_right) >= rank(r));
            }
        }

        #[test]
        fn volume_alone_never_changes_a_rank(r in record(), times in 1u32..5) {
            // The same accuracy over more answers ranks the same, once measured.
            prop_assume!(r.scored >= MIN_SCORED);
            let more = Record { scored: r.scored * times, right: r.right * times, ..r };
            prop_assert_eq!(rank(more), rank(r));
        }

        #[test]
        fn the_board_order_is_the_same_whatever_order_rows_arrive_in(
            rows in proptest::collection::vec(("[a-e]{1,2}", 0u32..60, any::<bool>()), 0..12),
            seed in any::<u64>(),
        ) {
            let standings: Vec<Standing> = rows
                .into_iter()
                .map(|(handle, scored, flagged)| Standing { handle, scored, right: scored / 2, flagged })
                .collect();
            let mut shuffled = standings.clone();
            // A cheap deterministic shuffle.
            shuffled.sort_by_key(|s| (seed ^ s.scored as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let board = leaderboard(standings);
            prop_assert_eq!(&board, &leaderboard(shuffled));
            prop_assert!(board.iter().all(|s| !s.flagged && s.scored >= MIN_WEEKLY_SCORED));
        }
    }
}
