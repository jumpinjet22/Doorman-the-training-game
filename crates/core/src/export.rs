//! Training data from what the crowd decided: one row per labelled item,
//! ready for a training script. Intent items become classification rows;
//! pair items become preference rows (`prompt` / `chosen` / `rejected`, the
//! shape DPO trainers read).
//!
//! A gold item is labelled with its known answer, whatever the crowd said.
//! Anything else is exported only once the scorer has settled it: open and
//! disputed items are left out, as is a pair the crowd judged `both_bad`,
//! which has no reply to prefer. Votes the scorer zeroed (brigades, cohorts)
//! are already missing from the split, so they never reach a row.
//!
//! The rows hold gold answers: an export is for training, never for serving.

use serde::Serialize;

use crate::batch::{BatchItem, BOTH_BAD};

pub const EXPORT_FORMAT: &str = "dorman-export";
pub const EXPORT_VERSION: u32 = 1;

/// What the scorer decided about an item, as stored.
#[derive(Debug, Clone, Copy)]
pub struct Crowd<'a> {
    /// `open`, `settled` or `disputed`.
    pub status: &'a str,
    pub label: Option<&'a str>,
    /// Answer -> trust behind it, brigade and cohort votes left out.
    pub split: &'a [(String, f64)],
}

/// Where a row's label came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The item's known answer.
    Gold,
    /// The scorer's settled verdict.
    Crowd,
}

/// One intent item, labelled.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IntentRow {
    pub id: String,
    pub text: String,
    pub scene: Option<String>,
    pub label: String,
    pub source: Source,
    /// What the generator was asked to write; differs from `label` when the
    /// crowd (or the gold answer) disagreed with it.
    pub intended_intent: String,
    /// Share of the crowd's trust behind `label`, if anyone weighed in.
    pub agreement: Option<f64>,
    /// Total trust behind every answer, if anyone weighed in.
    pub weight: Option<f64>,
    pub votes: i64,
    pub batch: String,
}

/// One pair item as a preference: the reply the crowd (or gold) chose over
/// the other.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DpoRow {
    pub id: String,
    pub prompt: String,
    pub chosen: String,
    pub rejected: String,
    pub text: String,
    pub scene: Option<String>,
    pub source: Source,
    pub agreement: Option<f64>,
    pub weight: Option<f64>,
    pub votes: i64,
    pub batch: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Intent(IntentRow),
    Dpo(DpoRow),
}

/// Why an item isn't exported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Not scored yet, or still open.
    Open,
    Disputed,
    /// A pair where neither reply was any good: nothing to prefer.
    BothBad,
}

/// The prompt a pair's replies answer: the scene, if any, and what the
/// visitor said.
pub fn prompt(text: &str, scene: Option<&str>) -> String {
    match scene {
        Some(scene) => format!("Scene: {scene}\nVisitor: \"{text}\""),
        None => format!("Visitor: \"{text}\""),
    }
}

/// `label`'s share of the split, and the split's total, if it has any weight.
fn agreement(split: &[(String, f64)], label: &str) -> (Option<f64>, Option<f64>) {
    let total: f64 = split.iter().map(|(_, w)| w).sum();
    if total <= 0.0 {
        return (None, None);
    }
    let behind: f64 = split
        .iter()
        .filter(|(answer, _)| answer == label)
        .map(|(_, w)| w)
        .sum();
    (Some(behind / total), Some(total))
}

/// The row for one item, or why it has none.
pub fn row(item: &BatchItem, batch: &str, crowd: Option<Crowd>, votes: i64) -> Result<Row, Skip> {
    let (label, source) = match item.gold_answer() {
        Some(gold) => (gold, Source::Gold),
        None => match crowd {
            Some(Crowd {
                status: "settled",
                label: Some(label),
                ..
            }) => (label, Source::Crowd),
            Some(Crowd {
                status: "disputed", ..
            }) => return Err(Skip::Disputed),
            _ => return Err(Skip::Open),
        },
    };
    let split = crowd.map_or(&[][..], |c| c.split);
    let (agreement, weight) = agreement(split, label);
    match item {
        BatchItem::Intent(intent) => Ok(Row::Intent(IntentRow {
            id: intent.item_key.clone(),
            text: intent.text.clone(),
            scene: intent.scene.clone(),
            label: label.to_string(),
            source,
            intended_intent: intent.intended_intent.clone(),
            agreement,
            weight,
            votes,
            batch: batch.to_string(),
        })),
        BatchItem::Pair(pair) => {
            if label == BOTH_BAD {
                return Err(Skip::BothBad);
            }
            let (chosen, rejected): (Vec<_>, Vec<_>) =
                pair.replies.iter().partition(|r| r.item_key == label);
            // A validated batch has two replies and a label naming one.
            let ([chosen], [rejected]) = (chosen.as_slice(), rejected.as_slice()) else {
                return Err(Skip::Open);
            };
            Ok(Row::Dpo(DpoRow {
                id: pair.item_key.clone(),
                prompt: prompt(&pair.text, pair.scene.as_deref()),
                chosen: chosen.text.clone(),
                rejected: rejected.text.clone(),
                text: pair.text.clone(),
                scene: pair.scene.clone(),
                source,
                agreement,
                weight,
                votes,
                batch: batch.to_string(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{IntentItem, PairItem, Reply};

    fn intent(gold: Option<&str>) -> BatchItem {
        BatchItem::Intent(IntentItem {
            item_key: "k1".into(),
            text: "Package for you".into(),
            scene: None,
            intended_intent: "delivery".into(),
            gold_answer: gold.map(str::to_string),
            explanation: None,
        })
    }

    fn pair(gold: Option<&str>, scene: Option<&str>) -> BatchItem {
        BatchItem::Pair(PairItem {
            item_key: "pair:a:b".into(),
            text: "Is anyone home?".into(),
            scene: scene.map(str::to_string),
            intended_intent: "person_lookup".into(),
            gold_answer: gold.map(str::to_string),
            replies: vec![
                Reply {
                    item_key: "a".into(),
                    text: "Who are you looking for?".into(),
                },
                Reply {
                    item_key: "b".into(),
                    text: "Nobody's home, come in.".into(),
                },
            ],
            explanation: None,
        })
    }

    fn split(pairs: &[(&str, f64)]) -> Vec<(String, f64)> {
        pairs.iter().map(|(a, w)| (a.to_string(), *w)).collect()
    }

    fn settled<'a>(label: &'a str, split: &'a [(String, f64)]) -> Crowd<'a> {
        Crowd {
            status: "settled",
            label: Some(label),
            split,
        }
    }

    #[test]
    fn a_settled_intent_item_is_labelled_by_the_crowd() {
        let s = split(&[("delivery", 3.0), ("soliciting", 1.0)]);
        let Ok(Row::Intent(row)) = row(&intent(None), "b1", Some(settled("delivery", &s)), 7)
        else {
            panic!("expected an intent row");
        };
        assert_eq!(row.label, "delivery");
        assert_eq!(row.source, Source::Crowd);
        assert_eq!(row.agreement, Some(0.75));
        assert_eq!(row.weight, Some(4.0));
        assert_eq!(row.votes, 7);
        assert_eq!(row.batch, "b1");
        assert_eq!(row.intended_intent, "delivery");
    }

    #[test]
    fn gold_uses_its_known_answer_even_when_the_crowd_disagrees() {
        let s = split(&[("soliciting", 3.0), ("delivery", 1.0)]);
        let crowd = settled("soliciting", &s);
        let Ok(Row::Intent(row)) = row(&intent(Some("delivery")), "b1", Some(crowd), 4) else {
            panic!("expected an intent row");
        };
        assert_eq!(row.label, "delivery");
        assert_eq!(row.source, Source::Gold);
        // Agreement is with the exported label, not the crowd's.
        assert_eq!(row.agreement, Some(0.25));
    }

    #[test]
    fn gold_nobody_has_answered_is_still_exported() {
        let Ok(Row::Intent(row)) = row(&intent(Some("delivery")), "b1", None, 0) else {
            panic!("expected an intent row");
        };
        assert_eq!(row.label, "delivery");
        assert_eq!((row.agreement, row.weight), (None, None));
    }

    #[test]
    fn unsettled_items_are_skipped_with_a_reason() {
        let s = split(&[("delivery", 1.0)]);
        let open = Crowd {
            status: "open",
            label: None,
            split: &s,
        };
        let disputed = Crowd {
            status: "disputed",
            label: Some("delivery"),
            split: &s,
        };
        assert_eq!(row(&intent(None), "b", None, 0), Err(Skip::Open));
        assert_eq!(row(&intent(None), "b", Some(open), 1), Err(Skip::Open));
        assert_eq!(
            row(&intent(None), "b", Some(disputed), 1),
            Err(Skip::Disputed)
        );
    }

    #[test]
    fn a_settled_pair_prefers_the_chosen_reply() {
        let s = split(&[("b", 2.0), ("a", 6.0)]);
        let Ok(Row::Dpo(row)) = row(&pair(None, None), "b1", Some(settled("a", &s)), 9) else {
            panic!("expected a dpo row");
        };
        assert_eq!(row.chosen, "Who are you looking for?");
        assert_eq!(row.rejected, "Nobody's home, come in.");
        assert_eq!(row.prompt, "Visitor: \"Is anyone home?\"");
        assert_eq!(row.agreement, Some(0.75));
        assert_eq!(row.source, Source::Crowd);
    }

    #[test]
    fn a_gold_pair_prefers_its_known_reply() {
        let Ok(Row::Dpo(row)) = row(
            &pair(Some("b"), Some("Night, porch light off")),
            "b1",
            None,
            0,
        ) else {
            panic!("expected a dpo row");
        };
        assert_eq!(row.chosen, "Nobody's home, come in.");
        assert_eq!(row.source, Source::Gold);
        assert_eq!(
            row.prompt,
            "Scene: Night, porch light off\nVisitor: \"Is anyone home?\""
        );
    }

    #[test]
    fn both_bad_has_nothing_to_prefer() {
        let s = split(&[(BOTH_BAD, 5.0)]);
        let crowd = settled(BOTH_BAD, &s);
        assert_eq!(
            row(&pair(None, None), "b", Some(crowd), 5),
            Err(Skip::BothBad)
        );
        assert_eq!(
            row(&pair(Some(BOTH_BAD), None), "b", None, 0),
            Err(Skip::BothBad)
        );
    }

    #[test]
    fn rows_serialise_with_the_documented_fields() {
        let s = split(&[("delivery", 1.0)]);
        let Ok(Row::Intent(row)) = row(&intent(None), "b1", Some(settled("delivery", &s)), 1)
        else {
            panic!("expected an intent row");
        };
        let json = serde_json::to_value(&row).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "agreement",
                "batch",
                "id",
                "intended_intent",
                "label",
                "scene",
                "source",
                "text",
                "votes",
                "weight"
            ]
        );
        assert_eq!(json["source"], "crowd");
    }
}
