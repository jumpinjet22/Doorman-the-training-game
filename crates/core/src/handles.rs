//! Leaderboard handles: the shape rules, plus the names a player can't take
//! because they would look like the game speaking, or are abusive.
//!
//! Every check runs on a normalised form of the handle: lowercased, `_` and
//! `-` dropped, and common digit-for-letter swaps undone, so `Adm1n`,
//! `KN0CK` and `head-of-security` are caught the same as `admin`, `knock`
//! and `headofsecurity`.

/// Why a handle was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleProblem {
    /// Not 3-20 letters, digits, `_` or `-` starting with a letter or digit.
    Shape,
    /// Reads as the game, its staff or one of its ranks.
    Reserved,
    /// Contains a word on the blocklist.
    Blocked,
}

/// Names that would let a player appear to speak for the game. Matched
/// against the whole normalised handle.
const RESERVED_EXACT: &[&str] = &[
    "admin",
    "administrator",
    "anonymous",
    "bouncer",
    "doorman",
    "headofsecurity",
    "mod",
    "moderator",
    "null",
    "official",
    "root",
    "rookie",
    "security",
    "staff",
    "support",
    "system",
    "team",
    "undefined",
];

/// Reserved words that can't appear anywhere in a handle: `knock_fan` or
/// `the_admin` would still read as official.
const RESERVED_ANYWHERE: &[&str] = &[
    "admin",
    "dorman",
    "doorman",
    "knock",
    "moderator",
    "official",
];

/// A short blocklist of slurs and sexual terms, deliberately small: the
/// maintainer can clear any handle that gets past it
/// (`dorman-ingest clear-handle`). Words distinctive enough not to turn up
/// inside ordinary words are matched anywhere in the handle...
const BLOCKED_ANYWHERE: &[&str] = &["faggot", "nigg", "porn", "retard", "tranny", "whore"];

/// ...and the rest only as a whole word between `_`/`-` separators, so
/// "suspicious", "grapefruit" and "Scunthorpe" stay allowed.
const BLOCKED_WORDS: &[&str] = &[
    "chink", "cunt", "fag", "kike", "nazi", "penis", "rape", "spic",
];

/// The handle as the checks see it.
pub fn normalise(handle: &str) -> String {
    handle
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .map(|c| match c.to_ascii_lowercase() {
            '0' => 'o',
            '1' => 'i',
            '3' => 'e',
            '4' => 'a',
            '5' => 's',
            '7' => 't',
            '8' => 'b',
            other => other,
        })
        .collect()
}

fn shape_ok(handle: &str) -> bool {
    (3..=20).contains(&handle.len())
        && handle.starts_with(|c: char| c.is_ascii_alphanumeric())
        && handle
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// None if `handle` may be used on the leaderboard.
pub fn check_handle(handle: &str) -> Option<HandleProblem> {
    if !shape_ok(handle) {
        return Some(HandleProblem::Shape);
    }
    let norm = normalise(handle);
    // "1" undone as "i" misses "l" lookalikes (`adm1n` vs `ki11`); check both.
    let alt = norm.replace('i', "l");
    let forms = [norm.as_str(), alt.as_str()];
    let words = handle.split(['_', '-']).map(normalise);
    let blocked_word = words
        .flat_map(|w| [w.replace('i', "l"), w])
        .any(|w| BLOCKED_WORDS.contains(&w.as_str()));
    if blocked_word
        || forms
            .iter()
            .any(|f| BLOCKED_ANYWHERE.iter().any(|w| f.contains(w)))
    {
        return Some(HandleProblem::Blocked);
    }
    let reserved = forms
        .iter()
        .any(|f| RESERVED_EXACT.contains(f) || RESERVED_ANYWHERE.iter().any(|w| f.contains(w)));
    reserved.then_some(HandleProblem::Reserved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_handles_pass() {
        for ok in ["Ada", "Ada_Lovelace", "bob-42", "x1y2z3", "Porch_Pirate_99"] {
            assert_eq!(check_handle(ok), None, "{ok}");
        }
    }

    #[test]
    fn the_shape_rules_still_apply() {
        for bad in [
            "ab",
            "_ada",
            "-ada",
            "has space",
            "émile",
            &"a".repeat(21),
            "",
        ] {
            assert_eq!(check_handle(bad), Some(HandleProblem::Shape), "{bad:?}");
        }
    }

    #[test]
    fn the_game_its_staff_and_its_ranks_are_reserved() {
        for name in [
            "KNOCK",
            "KN0CK",
            "knock_fan",
            "Dorman",
            "Doorman",
            "admin",
            "Adm1n",
            "the-admin",
            "Head-of-Security",
            "Bouncer",
            "Staff",
            "0fficial",
            "Moderator",
        ] {
            assert_eq!(check_handle(name), Some(HandleProblem::Reserved), "{name}");
        }
    }

    #[test]
    fn exact_only_words_are_fine_inside_longer_names() {
        // "staff" or "root" alone reads as official; inside a name it doesn't.
        for ok in ["Rootbeer", "Staffordshire", "Teamwork", "Modest_Mouse"] {
            assert_eq!(check_handle(ok), None, "{ok}");
        }
    }

    #[test]
    fn blocked_words_are_caught_through_lookalikes_and_separators() {
        for name in ["n4zi_fan", "the-NAZI", "pr0n_porn", "xxPORNxx", "ret4rd"] {
            assert_eq!(check_handle(name), Some(HandleProblem::Blocked), "{name}");
        }
    }

    #[test]
    fn ambiguous_words_are_only_blocked_on_their_own() {
        for ok in [
            "suspicious_guy",
            "grapefruit",
            "Scunthorpe",
            "Nazir",
            "Dickens_fan",
        ] {
            assert_eq!(check_handle(ok), None, "{ok}");
        }
    }

    #[test]
    fn normalising_undoes_separators_case_and_digit_swaps() {
        assert_eq!(normalise("Head-of_Security"), "headofsecurity");
        assert_eq!(normalise("KN0CK"), "knock");
        assert_eq!(normalise("4dm1n"), "admin");
    }
}
