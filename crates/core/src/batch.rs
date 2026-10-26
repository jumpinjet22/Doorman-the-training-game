//! The `knock-game-batch` v1 file that KNOCK's `scripts/export_game_batch.py`
//! writes. That script's docstring is the spec; this is the Rust reading of it.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const BATCH_FORMAT: &str = "knock-game-batch";
pub const BATCH_VERSION: u32 = 1;
/// A pair item's answer when neither reply is any good.
pub const BOTH_BAD: &str = "both_bad";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    pub format: String,
    pub version: u32,
    pub batch: String,
    pub intents: Vec<String>,
    pub items: Vec<BatchItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BatchItem {
    Intent(IntentItem),
    Pair(PairItem),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentItem {
    pub knock_key: String,
    pub text: String,
    pub scene: Option<String>,
    pub generator_intent: String,
    pub gold_answer: Option<String>,
    pub judge: Option<Judge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairItem {
    pub knock_key: String,
    pub text: String,
    pub scene: Option<String>,
    pub generator_intent: String,
    pub gold_answer: Option<String>,
    pub replies: Vec<Reply>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub knock_key: String,
    pub text: String,
    pub judge: Option<Judge>,
}

/// KNOCK's judge-ensemble averages (0-10) for one entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Judge {
    pub visitor_voice: f64,
    pub category_correct: f64,
    pub safety_compliant: f64,
    pub natural_quality: f64,
    pub safety_veto: bool,
    pub disagreement: f64,
}

#[derive(Debug, Error)]
pub enum BatchError {
    #[error("not valid batch JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("expected {BATCH_FORMAT} v{BATCH_VERSION}, got {0} v{1}")]
    Format(String, u32),
    #[error("item {0}: {1}")]
    Item(String, String),
}

impl BatchItem {
    pub fn knock_key(&self) -> &str {
        match self {
            BatchItem::Intent(item) => &item.knock_key,
            BatchItem::Pair(item) => &item.knock_key,
        }
    }

    pub fn mode(&self) -> &'static str {
        match self {
            BatchItem::Intent(_) => "intent",
            BatchItem::Pair(_) => "pair",
        }
    }

    pub fn gold_answer(&self) -> Option<&str> {
        match self {
            BatchItem::Intent(item) => item.gold_answer.as_deref(),
            BatchItem::Pair(item) => item.gold_answer.as_deref(),
        }
    }

    /// Whether `answer` is something a player could legitimately submit for
    /// this item. Intent options are checked separately, against the options
    /// the player was actually shown.
    pub fn accepts(&self, answer: &str, intents: &[String]) -> bool {
        match self {
            BatchItem::Intent(_) => intents.iter().any(|intent| intent == answer),
            BatchItem::Pair(item) => {
                answer == BOTH_BAD || item.replies.iter().any(|reply| reply.knock_key == answer)
            }
        }
    }
}

impl Batch {
    pub fn parse(json: &str) -> Result<Self, BatchError> {
        let batch: Batch = serde_json::from_str(json)?;
        batch.validate()?;
        Ok(batch)
    }

    fn validate(&self) -> Result<(), BatchError> {
        if self.format != BATCH_FORMAT || self.version != BATCH_VERSION {
            return Err(BatchError::Format(self.format.clone(), self.version));
        }
        let known = |intent: &str| self.intents.iter().any(|known| known == intent);
        let mut seen = HashSet::new();
        for item in &self.items {
            let key = item.knock_key();
            let bad = |why: &str| Err(BatchError::Item(key.to_string(), why.to_string()));
            if !seen.insert(key) {
                return bad("duplicate knock_key");
            }
            match item {
                BatchItem::Intent(intent) => {
                    if !known(&intent.generator_intent) {
                        return bad("generator_intent is not in the intent list");
                    }
                    if intent
                        .gold_answer
                        .as_deref()
                        .is_some_and(|gold| !known(gold))
                    {
                        return bad("gold_answer is not in the intent list");
                    }
                }
                BatchItem::Pair(pair) => {
                    let [a, b] = pair.replies.as_slice() else {
                        return bad("a pair needs exactly two replies");
                    };
                    if key != format!("pair:{}:{}", a.knock_key, b.knock_key) {
                        return bad("pair key doesn't match its replies");
                    }
                    if let Some(gold) = &pair.gold_answer {
                        if !item.accepts(gold, &self.intents) {
                            return bad("gold_answer is neither reply");
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../fixtures/batch.example.json");

    #[test]
    fn parses_the_example_batch_from_knock() {
        let batch = Batch::parse(EXAMPLE).unwrap();
        assert_eq!(batch.batch, "example-1");
        assert!(batch.items.iter().any(|item| item.mode() == "pair"));
        assert!(batch.items.iter().any(|item| item.gold_answer().is_some()));
    }

    #[test]
    fn rejects_a_different_format_or_version() {
        let json = EXAMPLE.replace("\"version\": 1", "\"version\": 2");
        assert!(matches!(Batch::parse(&json), Err(BatchError::Format(_, 2))));
    }

    #[test]
    fn rejects_an_unknown_gold_intent() {
        let mut batch = Batch::parse(EXAMPLE).unwrap();
        let BatchItem::Intent(item) = &mut batch.items[0] else {
            panic!("expected intent")
        };
        item.gold_answer = Some("pizza_party".into());
        let json = serde_json::to_string(&batch).unwrap();
        assert!(matches!(Batch::parse(&json), Err(BatchError::Item(..))));
    }

    #[test]
    fn pair_answers_are_a_reply_key_or_both_bad() {
        let batch = Batch::parse(EXAMPLE).unwrap();
        let pair = batch
            .items
            .iter()
            .find(|item| item.mode() == "pair")
            .unwrap();
        let BatchItem::Pair(inner) = pair else {
            unreachable!()
        };
        assert!(pair.accepts(&inner.replies[0].knock_key, &batch.intents));
        assert!(pair.accepts(BOTH_BAD, &batch.intents));
        assert!(!pair.accepts("delivery", &batch.intents));
    }
}
