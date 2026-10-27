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

/// A wrong answer that at least this share of the crowd's weight agreed
/// with counts as close.
pub const CLOSE_SHARE: f64 = 0.25;

/// What the crowd decided about one item, once the scorer has run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Decision {
    /// The settled answer, if the item settled.
    pub settled: Option<String>,
    /// Answer -> trust behind it.
    pub split: Vec<(String, f64)>,
}

/// How `answer` scored. Everything is pending until the night's scoring has
/// run (`results_out`), even gold, so a card never gives away which items
/// had a known answer while the day is still being played. Gold items are
/// scored against gold; other items against the settled crowd answer, and
/// stay pending if the crowd hasn't settled.
pub fn score_answer(
    answer: &str,
    gold: Option<&str>,
    decision: Option<&Decision>,
    results_out: bool,
) -> Outcome {
    if !results_out {
        return Outcome::Pending;
    }
    if let Some(gold) = gold {
        return if answer == gold {
            Outcome::Right
        } else {
            Outcome::Wrong
        };
    }
    let Some(decision) = decision else {
        return Outcome::Pending;
    };
    let Some(settled) = &decision.settled else {
        return Outcome::Pending;
    };
    if answer == settled {
        return Outcome::Right;
    }
    let total: f64 = decision.split.iter().map(|(_, w)| w).sum();
    let mine: f64 = decision
        .split
        .iter()
        .filter(|(a, _)| a == answer)
        .map(|(_, w)| w)
        .sum();
    if total > 0.0 && mine / total >= CLOSE_SHARE {
        Outcome::Close
    } else {
        Outcome::Wrong
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

    fn settled(label: &str, split: &[(&str, f64)]) -> Decision {
        Decision {
            settled: Some(label.into()),
            split: split.iter().map(|(a, w)| (a.to_string(), *w)).collect(),
        }
    }

    #[test]
    fn nothing_is_scored_until_the_night_run() {
        let decision = settled("a", &[("a", 1.0)]);
        assert_eq!(score_answer("a", Some("a"), None, false), Outcome::Pending);
        assert_eq!(
            score_answer("a", None, Some(&decision), false),
            Outcome::Pending
        );
    }

    #[test]
    fn answers_score_against_gold_or_the_settled_crowd() {
        assert_eq!(score_answer("a", Some("a"), None, true), Outcome::Right);
        assert_eq!(score_answer("b", Some("a"), None, true), Outcome::Wrong);

        let decision = settled("a", &[("a", 6.0), ("b", 2.5), ("c", 1.5)]);
        assert_eq!(
            score_answer("a", None, Some(&decision), true),
            Outcome::Right
        );
        assert_eq!(
            score_answer("b", None, Some(&decision), true),
            Outcome::Close
        );
        assert_eq!(
            score_answer("c", None, Some(&decision), true),
            Outcome::Wrong
        );
        assert_eq!(
            score_answer("d", None, Some(&decision), true),
            Outcome::Wrong
        );

        let open = Decision {
            settled: None,
            split: vec![("a".into(), 1.0)],
        };
        assert_eq!(score_answer("a", None, Some(&open), true), Outcome::Pending);
        assert_eq!(score_answer("a", None, None, true), Outcome::Pending);
    }
}

#[cfg(test)]
mod properties {
    use proptest::prelude::*;

    use super::*;

    fn outcome() -> impl Strategy<Value = Outcome> {
        prop_oneof![
            Just(Outcome::Right),
            Just(Outcome::Close),
            Just(Outcome::Wrong),
            Just(Outcome::Pending),
        ]
    }

    proptest! {
        /// The card is safe to post anywhere: it's made only of the door, the
        /// day number, one square per item and the streak, so it can't carry
        /// an item's text or anyone's answer.
        #[test]
        fn the_card_holds_no_spoilers(
            day in 1u32..100_000, outcomes in proptest::collection::vec(outcome(), 0..20),
            streak in 0u32..1000,
        ) {
            let card = share_card(day, &outcomes, streak);
            let mut lines = card.lines();
            prop_assert_eq!(
                lines.next().unwrap(),
                format!("🚪 #{day}  {}", outcomes.iter().map(|o| o.square()).collect::<String>())
            );
            match lines.next() {
                None => prop_assert!(streak <= 1),
                Some(line) => prop_assert_eq!(line, format!("🔥 {streak}-day streak")),
            }
            prop_assert!(lines.next().is_none());
            prop_assert!(card.chars().all(|c| c.is_ascii_digit()
                || " #\n-🚪🟩🟨🟥⬜🔥daystrek".contains(c)));
        }
    }
}
