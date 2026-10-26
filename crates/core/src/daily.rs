//! Picking each day's set, and the multiple-choice options for intent items.
//! Everything here is deterministic in its seed, so a rebuild of the same
//! day gives the same set and every player sees the same option order.

use sha2::{Digest, Sha256};

/// How many of each kind of item go into one daily set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DailyPlan {
    /// Items nobody has answered yet: this is the data.
    pub new: usize,
    /// Items with a known answer, used to measure each player.
    pub gold: usize,
    /// Yesterday's most disputed items, to firm them up.
    pub carry_over: usize,
}

impl Default for DailyPlan {
    fn default() -> Self {
        Self {
            new: 6,
            gold: 3,
            carry_over: 1,
        }
    }
}

impl DailyPlan {
    pub fn total(&self) -> usize {
        self.new + self.gold + self.carry_over
    }
}

fn rank(seed: &str, value: &str) -> [u8; 32] {
    Sha256::new()
        .chain_update(seed)
        .chain_update([0])
        .chain_update(value)
        .finalize()
        .into()
}

/// `values` in a stable pseudo-random order derived from `seed`.
pub fn seeded_shuffle<T: AsRef<str>>(seed: &str, mut values: Vec<T>) -> Vec<T> {
    values.sort_by_cached_key(|value| rank(seed, value.as_ref()));
    values
}

/// The knock_keys for one day's set, gold mixed in so it can't be told apart.
///
/// `carry_over` should already be sorted most-disputed first. A pool that's
/// short is topped up from `new` so the set stays full when there's enough
/// data overall.
pub fn pick_daily_set(
    seed: &str,
    plan: DailyPlan,
    new: &[String],
    gold: &[String],
    carry_over: &[String],
) -> Vec<String> {
    let mut picked: Vec<String> = carry_over.iter().take(plan.carry_over).cloned().collect();
    let gold_pool = seeded_shuffle(seed, gold.iter().filter(|k| !picked.contains(k)).collect());
    picked.extend(gold_pool.into_iter().take(plan.gold).cloned());
    let wanted = plan.total().saturating_sub(picked.len());
    let new_pool = seeded_shuffle(seed, new.iter().filter(|k| !picked.contains(k)).collect());
    picked.extend(new_pool.into_iter().take(wanted).cloned());
    seeded_shuffle(seed, picked)
}

/// The four choices shown for an intent item: the generator's label, the
/// gold answer when it differs, and plausible distractors, in a fixed order.
pub fn intent_options(
    knock_key: &str,
    generator_intent: &str,
    gold_answer: Option<&str>,
    intents: &[String],
    count: usize,
) -> Vec<String> {
    let mut options = vec![generator_intent.to_string()];
    if let Some(gold) = gold_answer.filter(|gold| *gold != generator_intent) {
        options.push(gold.to_string());
    }
    let distractors = intents
        .iter()
        .filter(|intent| !options.contains(intent))
        .collect();
    let wanted = count.saturating_sub(options.len());
    options.extend(
        seeded_shuffle(knock_key, distractors)
            .into_iter()
            .take(wanted)
            .cloned(),
    );
    seeded_shuffle(knock_key, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(prefix: &str, n: usize) -> Vec<String> {
        (0..n).map(|i| format!("{prefix}{i}")).collect()
    }

    #[test]
    fn a_full_set_has_the_planned_mix() {
        let (new, gold, carry) = (keys("n", 20), keys("g", 5), keys("c", 3));
        let set = pick_daily_set("2026-10-27", DailyPlan::default(), &new, &gold, &carry);

        assert_eq!(set.len(), 10);
        assert_eq!(set.iter().filter(|k| k.starts_with('n')).count(), 6);
        assert_eq!(set.iter().filter(|k| k.starts_with('g')).count(), 3);
        assert!(set.contains(&"c0".to_string()));
    }

    #[test]
    fn the_same_day_gives_the_same_set_and_another_day_differs() {
        let (new, gold) = (keys("n", 50), keys("g", 10));
        let plan = DailyPlan::default();
        let a = pick_daily_set("2026-10-27", plan, &new, &gold, &[]);
        assert_eq!(a, pick_daily_set("2026-10-27", plan, &new, &gold, &[]));
        assert_ne!(a, pick_daily_set("2026-10-28", plan, &new, &gold, &[]));
    }

    #[test]
    fn short_pools_are_topped_up_from_new_items() {
        let set = pick_daily_set(
            "d",
            DailyPlan::default(),
            &keys("n", 20),
            &keys("g", 1),
            &[],
        );
        assert_eq!(set.len(), 10);
    }

    #[test]
    fn intent_options_always_include_generator_and_gold() {
        let intents = keys("i", 14);
        let options = intent_options("k1", "i3", Some("i7"), &intents, 4);
        assert_eq!(options.len(), 4);
        assert!(options.contains(&"i3".to_string()));
        assert!(options.contains(&"i7".to_string()));
        assert_eq!(options, intent_options("k1", "i3", Some("i7"), &intents, 4));
    }
}
