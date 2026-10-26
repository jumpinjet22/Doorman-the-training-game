//! What a browser is allowed to see. A stored item carries its gold answer,
//! the generator's label and judge scores; none of that leaves through here.

use askama::Template;
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
}

#[derive(Template)]
#[template(path = "play.html")]
pub struct PlayPage {
    pub day: String,
    pub items: Vec<PublicItem>,
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
        BatchItem::Pair(pair) => match pair.replies.iter().position(|r| r.knock_key == answer) {
            Some(0) => "Reply A".to_string(),
            Some(_) => "Reply B".to_string(),
            None if answer == BOTH_BAD => "Both bad".to_string(),
            None => answer.to_string(),
        },
    }
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
                answered,
            },
            BatchItem::Pair(item) => {
                let mut choices: Vec<Choice> = item
                    .replies
                    .iter()
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
    fn the_page_never_contains_gold_or_judge_data() {
        let batch = Batch::parse(EXAMPLE).unwrap();
        let items = batch
            .items
            .iter()
            .map(|item| PublicItem::from_stored(&stored(item, vec!["delivery".into()]), None))
            .collect();
        let html = PlayPage {
            day: "2026-10-27".into(),
            items,
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
        assert!(html.contains("Both bad"));
        assert!(html.contains("Dropping off a package"));
    }

    #[test]
    fn labels_cover_every_intent_in_the_batch() {
        let batch = Batch::parse(EXAMPLE).unwrap();
        for intent in &batch.intents {
            assert!(!intent_label(intent).contains('_'));
        }
    }
}
