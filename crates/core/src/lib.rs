//! Domain types and pure game logic. No IO lives here.

pub mod batch;
pub mod daily;
pub mod share;

pub use batch::{Batch, BatchError, BatchItem, BOTH_BAD};
pub use daily::{intent_options, pick_daily_set, DailyPlan};
pub use share::{share_card, Outcome};

/// How many choices an intent item shows.
pub const INTENT_OPTION_COUNT: usize = 4;
