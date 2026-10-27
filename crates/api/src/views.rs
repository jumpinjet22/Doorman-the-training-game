//! What a browser is allowed to see. A stored item carries its gold answer,
//! the generator's label and judge scores; none of that leaves through here.

use askama::Template;
use dorman_core::batch::{PairItem, Reply};
use dorman_core::daily::seeded_shuffle;
use dorman_core::{BatchItem, BOTH_BAD};
use dorman_db::StoredItem;
use uuid::Uuid;

pub struct Choice {
    pub value: String,
    pub label: String,
}

pub struct PublicItem {
    pub id: Uuid,
    pub prompt: &'static str,
    pub text: String,
    pub scene: Option<String>,
    pub choices: Vec<Choice>,
    pub answered: Option<String>,
    /// Why the answer is what it is. Set only once the item is answered, so
    /// an unanswered card can never carry it.
    pub explanation: Option<String>,
}

/// The day at the top of every game page: "Day 12 · 2026-10-27".
pub struct Masthead {
    pub day: String,
    /// None before launch.
    pub number: Option<u32>,
}

/// One answer's share of the crowd, as a bar.
pub struct Bar {
    pub label: String,
    pub percent: u32,
    pub mine: bool,
}

/// Bars for `split` (answer, weight or count), labelled by `label`,
/// heaviest first, marking `mine`. Empty when nobody's answered.
pub fn bars(
    split: &[(String, f64)],
    label: impl Fn(&str) -> String,
    mine: Option<&str>,
) -> Vec<Bar> {
    let total: f64 = split.iter().map(|(_, weight)| weight).sum();
    if total <= 0.0 {
        return Vec::new();
    }
    let mut bars: Vec<(f64, Bar)> = split
        .iter()
        .map(|(answer, weight)| {
            let bar = Bar {
                label: label(answer),
                percent: (100.0 * weight / total).round() as u32,
                mine: mine == Some(answer.as_str()),
            };
            (*weight, bar)
        })
        .collect();
    bars.sort_by(|a, b| b.0.total_cmp(&a.0));
    bars.into_iter().map(|(_, bar)| bar).collect()
}

/// One card at a time: unanswered with its form, or (after a plain form
/// post) answered, with the crowd's split and a link on.
#[derive(Template)]
#[template(path = "card.html")]
pub struct CardPage {
    pub masthead: Masthead,
    /// 1-based.
    pub position: usize,
    pub total: usize,
    pub item: PublicItem,
    /// When this card was rendered, Unix milliseconds, for timing a form post.
    pub shown_at: i64,
    pub turnstile_site_key: String,
    /// Set once the card is answered.
    pub split: Option<Vec<Bar>>,
    /// Whether this was the last card.
    pub last: bool,
    /// In Overtime, how many Overtime answers the player has given.
    pub overtime: Option<i64>,
}

#[derive(Template)]
#[template(path = "empty.html")]
pub struct EmptyPage {
    pub masthead: Masthead,
    /// RFC 3339.
    pub reset_at: String,
    /// "5 h 12 min".
    pub reset_in: String,
}

pub struct ResultRow {
    pub item: PublicItem,
    pub mine: String,
    /// How sure they said they were, in words.
    pub sure: Option<&'static str>,
    pub bars: Vec<Bar>,
}

/// One line of the "how sure you were" table.
pub struct SureRow {
    pub label: &'static str,
    pub right: u32,
    pub close: u32,
    pub wrong: u32,
    pub pending: u32,
}

#[derive(Template)]
#[template(path = "results.html")]
pub struct ResultsPage {
    pub masthead: Masthead,
    pub rows: Vec<ResultRow>,
    pub card: String,
    pub streak: u32,
    /// Their leaderboard name, if they've picked one.
    pub handle: Option<String>,
    /// Whether any square is still waiting for the night's scoring.
    pub pending: bool,
    /// Confidence against outcome; empty if they never said how sure they were.
    pub sure: Vec<SureRow>,
    pub progress: Option<Progress>,
}

pub struct RevealRow {
    pub item: PublicItem,
    /// What became of it, in words.
    pub verdict: String,
    pub bars: Vec<Bar>,
}

#[derive(Template)]
#[template(path = "yesterday.html")]
pub struct YesterdayPage {
    pub masthead: Masthead,
    /// Whether there was a game yesterday.
    pub played_day: bool,
    /// Whether the night's scoring has run.
    pub out: bool,
    pub rows: Vec<RevealRow>,
    /// The player's own card for yesterday, if they played.
    pub card: Option<String>,
}

#[derive(Template)]
#[template(path = "about.html")]
pub struct AboutPage;

/// A heading, a sentence and a way on.
#[derive(Template)]
#[template(path = "message.html")]
pub struct MessagePage {
    pub heading: &'static str,
    pub message: &'static str,
    /// (href, text)
    pub link: (&'static str, &'static str),
}

/// One rank's board: players are compared with others of their own rank.
pub struct Tier {
    pub rank: &'static str,
    pub rows: Vec<BoardRow>,
}

/// Where a player stands, in words.
pub struct Progress {
    pub rank: &'static str,
    /// "72% of 31 settled answers", or None before any have settled.
    pub accuracy: Option<String>,
    /// What the next rank needs, e.g. "Bouncer: 4 more days and 75% accuracy".
    pub next: Option<String>,
}

pub struct BoardRow {
    pub place: usize,
    pub handle: String,
    pub percent: u32,
    pub scored: u32,
}

#[derive(Template)]
#[template(path = "leaderboard.html")]
pub struct LeaderboardPage {
    pub week_of: String,
    pub minimum: u32,
    pub tiers: Vec<Tier>,
}

#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorPage {
    pub message: &'static str,
}

/// Hours and minutes from now until then, e.g. "5 h 12 min".
pub fn until(seconds: i64) -> String {
    let minutes = (seconds.max(0) + 59) / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// A player-facing name for an intent.
pub fn intent_label(intent: &str) -> String {
    let known = match intent {
        "delivery" => "Dropping off a package",
        "delivery_signature_required" => "Package that needs a signature",
        "food_delivery" => "Food delivery",
        "religious_soliciting" => "Spreading the faith",
        "political_soliciting" => "Campaigning",
        "soliciting" => "Selling something",
        "ride_arrived" => "Your ride is here",
        "visitation" => "Here to visit someone",
        "service_appointment" => "Booked repair or service",
        "person_lookup" => "Looking for someone",
        "official_visit" => "Police or official business",
        "suspicious_activity" => "Something's off",
        "not_addressed" => "Not talking to the door",
        "unknown" => "No idea",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    let words = intent.replace('_', " ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The label for any answer to `item`, for showing the vote split.
pub fn choice_label(item: &BatchItem, answer: &str) -> String {
    match item {
        BatchItem::Intent(_) => intent_label(answer),
        BatchItem::Pair(pair) => match reply_order(pair).iter().position(|r| r.knock_key == answer)
        {
            Some(0) => "Reply A".to_string(),
            Some(_) => "Reply B".to_string(),
            None if answer == BOTH_BAD => "Both bad".to_string(),
            None => "Something else".to_string(),
        },
    }
}

/// A pair's replies in the order players see them, shuffled per item so
/// whichever reply KNOCK wrote first isn't always A. Stable, so everyone
/// sees the same order and a reload doesn't swap them.
pub fn reply_order(pair: &PairItem) -> Vec<&Reply> {
    let keys = seeded_shuffle(
        &pair.knock_key,
        pair.replies.iter().map(|r| r.knock_key.as_str()).collect(),
    );
    keys.iter()
        .filter_map(|key| pair.replies.iter().find(|r| r.knock_key == *key))
        .collect()
}

fn explanation(stored: &StoredItem, answered: &Option<String>) -> Option<String> {
    answered.as_ref()?;
    stored.item.explanation().map(str::to_string)
}

impl PublicItem {
    pub fn from_stored(stored: &StoredItem, answered: Option<String>) -> Self {
        match &stored.item {
            BatchItem::Intent(item) => PublicItem {
                id: stored.id,
                prompt: "Who's at the door?",
                text: item.text.clone(),
                scene: item.scene.clone(),
                choices: stored
                    .options
                    .iter()
                    .map(|o| Choice {
                        value: o.clone(),
                        label: intent_label(o),
                    })
                    .collect(),
                explanation: explanation(stored, &answered),
                answered,
            },
            BatchItem::Pair(item) => {
                let mut choices: Vec<Choice> = reply_order(item)
                    .into_iter()
                    .zip(["A", "B"])
                    .map(|(reply, letter)| Choice {
                        value: reply.knock_key.clone(),
                        label: format!("{letter}: \u{201c}{}\u{201d}", reply.text),
                    })
                    .collect();
                choices.push(Choice {
                    value: BOTH_BAD.to_string(),
                    label: "Both bad".to_string(),
                });
                PublicItem {
                    id: stored.id,
                    prompt: "Would you say that?",
                    text: item.text.clone(),
                    scene: item.scene.clone(),
                    choices,
                    explanation: explanation(stored, &answered),
                    answered,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dorman_core::Batch;

    const EXAMPLE: &str = include_str!("../../../fixtures/batch.example.json");

    fn stored(item: &BatchItem, options: Vec<String>) -> StoredItem {
        StoredItem {
            id: Uuid::nil(),
            knock_key: item.knock_key().into(),
            item: item.clone(),
            options,
        }
    }

    #[test]
    fn a_card_never_contains_gold_or_judge_data() {
        let batch = Batch::parse(EXAMPLE).unwrap();
        for item in &batch.items {
            let public = PublicItem::from_stored(&stored(item, vec!["delivery".into()]), None);
            let html = CardPage {
                masthead: Masthead {
                    day: "2026-10-27".into(),
                    number: Some(1),
                },
                position: 1,
                total: 1,
                item: public,
                shown_at: 0,
                turnstile_site_key: "site-key".into(),
                split: None,
                last: false,
                overtime: None,
            }
            .render()
            .unwrap();
            for field in [
                "gold",
                "generator",
                "judge",
                "category_correct",
                "safety",
                "knock_key",
            ] {
                assert!(!html.contains(field), "page mentions {field}");
            }
        }
    }

    #[test]
    fn bars_are_percentages_heaviest_first() {
        let split = vec![("a".to_string(), 1.0), ("b".to_string(), 3.0)];
        let got = bars(&split, |a| a.to_uppercase(), Some("a"));
        assert_eq!(got.len(), 2);
        assert_eq!(
            (got[0].label.as_str(), got[0].percent, got[0].mine),
            ("B", 75, false)
        );
        assert_eq!(
            (got[1].label.as_str(), got[1].percent, got[1].mine),
            ("A", 25, true)
        );
        assert!(bars(&[], |a| a.into(), None).is_empty());
    }

    #[test]
    fn countdowns_round_up_to_the_minute() {
        assert_eq!(until(0), "0 min");
        assert_eq!(until(1), "1 min");
        assert_eq!(until(3600), "1 h 0 min");
        assert_eq!(until(5 * 3600 + 11 * 60 + 30), "5 h 12 min");
    }

    #[test]
    fn pair_replies_are_shuffled_per_item_and_labelled_to_match() {
        let batch = Batch::parse(EXAMPLE).unwrap();
        let BatchItem::Pair(template) = batch
            .items
            .iter()
            .find(|i| matches!(i, BatchItem::Pair(_)))
            .unwrap()
        else {
            unreachable!()
        };
        // Over many items, the first reply KNOCK wrote lands on A about half the time.
        let mut first_is_a = 0;
        for n in 0..200 {
            let mut pair = template.clone();
            pair.knock_key = format!("pair:{n}");
            let order = reply_order(&pair);
            assert_eq!(order.len(), 2);
            assert_eq!(reply_order(&pair), order, "stable");
            if order[0].knock_key == pair.replies[0].knock_key {
                first_is_a += 1;
            }
            let item = BatchItem::Pair(pair.clone());
            let public = PublicItem::from_stored(&stored(&item, vec![]), None);
            assert_eq!(public.choices[0].value, order[0].knock_key);
            assert_eq!(choice_label(&item, &order[0].knock_key), "Reply A");
            assert_eq!(choice_label(&item, &order[1].knock_key), "Reply B");
        }
        assert!((60..140).contains(&first_is_a), "{first_is_a} of 200");
        let item = BatchItem::Pair(template.clone());
        assert_eq!(choice_label(&item, "both_bad"), "Both bad");
        assert_eq!(choice_label(&item, "some-raw-key"), "Something else");
    }

    #[test]
    fn labels_cover_every_intent_in_the_batch() {
        let batch = Batch::parse(EXAMPLE).unwrap();
        for intent in &batch.intents {
            assert!(!intent_label(intent).contains('_'));
        }
    }
}
