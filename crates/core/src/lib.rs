//! Domain types and pure game logic. No IO lives here.

pub mod batch;
pub mod calendar;
pub mod clock;
pub mod confidence;
pub mod daily;
pub mod handles;
pub mod limits;
pub mod ranks;
pub mod served;
pub mod share;
pub mod turnstile;

pub use batch::{Batch, BatchError, BatchItem, BOTH_BAD};
pub use calendar::{day_number, next_reset, streak};
pub use clock::{Clock, FixedClock, SystemClock};
pub use confidence::{tally, Confidence, Tally};
pub use daily::{intent_options, pick_daily_set, DailyPlan};
pub use handles::{check_handle, HandleProblem};
pub use ranks::{leaderboard, next_rank, rank, NextRank, Rank, Record, Standing};
pub use share::{score_answer, share_card, Decision, Outcome};
pub use turnstile::{TurnstileVerifier, Verdict};

/// How many choices an intent item shows.
pub const INTENT_OPTION_COUNT: usize = 4;
