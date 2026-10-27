//! The spoiler-free share card, e.g. `🚪 #142  🟩🟩🟨🟩⬜`.

/// How one answer scored against gold or the settled crowd answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Matched the gold or settled answer.
    Right,
    /// Didn't match, but a sizeable minority answered the same way.
    Close,
    Wrong,
    /// The item hasn't settled yet; the card fills in the next morning.
    Pending,
}

impl Outcome {
    fn square(self) -> char {
        match self {
            Outcome::Right => '🟩',
            Outcome::Close => '🟨',
            Outcome::Wrong => '🟥',
            Outcome::Pending => '⬜',
        }
    }
}

pub fn share_card(day_number: u32, outcomes: &[Outcome], streak: u32) -> String {
    let squares: String = outcomes.iter().map(|outcome| outcome.square()).collect();
    let mut card = format!("🚪 #{day_number}  {squares}");
    if streak > 1 {
        card.push_str(&format!("\n🔥 {streak}-day streak"));
    }
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_squares_and_a_streak() {
        let card = share_card(
            142,
            &[
                Outcome::Right,
                Outcome::Close,
                Outcome::Wrong,
                Outcome::Pending,
            ],
            3,
        );
        assert_eq!(card, "🚪 #142  🟩🟨🟥⬜\n🔥 3-day streak");
    }

    #[test]
    fn a_first_day_has_no_streak_line() {
        assert_eq!(share_card(1, &[Outcome::Right], 1), "🚪 #1  🟩");
    }
}
