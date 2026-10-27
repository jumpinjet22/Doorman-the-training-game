//! Timing answers on the server. Every card is rendered with a token that
//! says, under the server's signature, which item was served to whom and
//! when; the vote hands it back and the time to answer is measured from it.
//! Whatever the client says about its own timing is ignored, so a script
//! can't claim to have taken its time.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// Keeps these signatures apart from anything else signed with the same key.
const CONTEXT: &[u8] = b"dorman served v1";

fn mac(key: &[u8], item: &str, viewer: &str, served_ms: i64) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    for part in [CONTEXT, item.as_bytes(), viewer.as_bytes()] {
        mac.update(&(part.len() as u64).to_be_bytes());
        mac.update(part);
    }
    mac.update(&served_ms.to_be_bytes());
    mac
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// A token saying `item` was served to `viewer` (a player id, or "" before
/// they have one) at `served_ms`, Unix milliseconds.
pub fn sign(key: &[u8], item: &str, viewer: &str, served_ms: i64) -> String {
    let tag = mac(key, item, viewer, served_ms).finalize().into_bytes();
    format!("{served_ms}.{}", hex(&tag))
}

/// When `item` was served to `viewer`, if `token` is one of ours for exactly
/// that item and viewer.
pub fn served_at(key: &[u8], token: &str, item: &str, viewer: &str) -> Option<i64> {
    let (ms, tag) = token.split_once('.')?;
    let ms: i64 = ms.parse().ok()?;
    let tag = unhex(tag)?;
    mac(key, item, viewer, ms).verify_slice(&tag).ok()?;
    Some(ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"a key a key a key a key";

    #[test]
    fn a_token_gives_back_its_time_for_its_item_and_viewer_only() {
        let token = sign(KEY, "item", "player", 1_700_000_000_123);
        assert_eq!(
            served_at(KEY, &token, "item", "player"),
            Some(1_700_000_000_123)
        );
        assert_eq!(served_at(KEY, &token, "other", "player"), None);
        assert_eq!(served_at(KEY, &token, "item", ""), None);
        assert_eq!(served_at(b"another key", &token, "item", "player"), None);
    }

    #[test]
    fn a_changed_time_or_junk_is_refused() {
        let token = sign(KEY, "item", "", 1000);
        let (_, tag) = token.split_once('.').unwrap();
        assert_eq!(served_at(KEY, &format!("1.{tag}"), "item", ""), None);
        for junk in [
            "",
            "1000",
            "1000.",
            "x.y",
            "1000.zz",
            "1000.abc",
            &token[..10],
        ] {
            assert_eq!(served_at(KEY, junk, "item", ""), None, "{junk}");
        }
    }

    #[test]
    fn fields_cant_slide_into_each_other() {
        let token = sign(KEY, "ab", "c", 5);
        assert_eq!(served_at(KEY, &token, "a", "bc"), None);
    }
}
