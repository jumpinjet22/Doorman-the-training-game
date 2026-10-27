//! How sure a player said they were, and how that lined up with how right
//! they turned out to be. This is feedback for the player, never an input to
//! trust or consensus.

use crate::share::Outcome;

/// A player's stated confidence in one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Unsure,
    Fairly,
    Certain,
}

impl Confidence {
    pub const ALL: [Confidence; 3] = [Confidence::Unsure, Confidence::Fairly, Confidence::Certain];

    /// The value stored and sent over the wire.
    pub fn key(self) -> &'static str {
        match self {
            Confidence::Unsure => "unsure",
            Confidence::Fairly => "fairly",
            Confidence::Certain => "certain",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Confidence::Unsure => "Not sure",
            Confidence::Fairly => "Fairly sure",
            Confidence::Certain => "Certain",
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == key)
    }
}

/// Answers given at one confidence level, by how they turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub confidence: Confidence,
    pub right: u32,
    pub close: u32,
    pub wrong: u32,
    /// Not scored yet: the night's run hasn't happened, or the item is unsettled.
    pub pending: u32,
}

impl Tally {
    pub fn total(&self) -> u32 {
        self.right + self.close + self.wrong + self.pending
    }
}

/// Tallies (confidence, outcome) pairs, one row per level that was used, in
/// Unsure, Fairly, Certain order. Answers given without a confidence are left out.
pub fn tally(answers: &[(Option<Confidence>, Outcome)]) -> Vec<Tally> {
    Confidence::ALL
        .into_iter()
        .filter_map(|confidence| {
            let mut tally = Tally {
                confidence,
                right: 0,
                close: 0,
                wrong: 0,
                pending: 0,
            };
            for (_, outcome) in answers.iter().filter(|(c, _)| *c == Some(confidence)) {
                match outcome {
                    Outcome::Right => tally.right += 1,
                    Outcome::Close => tally.close += 1,
                    Outcome::Wrong => tally.wrong += 1,
                    Outcome::Pending => tally.pending += 1,
                }
            }
            (tally.total() > 0).then_some(tally)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_junk_is_refused() {
        for c in Confidence::ALL {
            assert_eq!(Confidence::parse(c.key()), Some(c));
        }
        assert_eq!(Confidence::parse("very"), None);
        assert_eq!(Confidence::parse(""), None);
    }

    #[test]
    fn tallies_by_level_and_skips_unanswered_levels() {
        let rows = tally(&[
            (Some(Confidence::Certain), Outcome::Right),
            (Some(Confidence::Certain), Outcome::Wrong),
            (Some(Confidence::Certain), Outcome::Pending),
            (Some(Confidence::Unsure), Outcome::Close),
            (None, Outcome::Right),
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].confidence, Confidence::Unsure);
        assert_eq!((rows[0].close, rows[0].total()), (1, 1));
        assert_eq!(rows[1].confidence, Confidence::Certain);
        assert_eq!((rows[1].right, rows[1].wrong, rows[1].pending), (1, 1, 1));
    }

    #[test]
    fn no_confidence_means_no_rows() {
        assert!(tally(&[(None, Outcome::Right)]).is_empty());
    }
}
