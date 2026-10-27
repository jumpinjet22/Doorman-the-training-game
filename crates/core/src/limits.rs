//! The pure half of rate limiting: fixed time windows, and the salted key a
//! client's network is counted under. The counters live in Postgres.

use std::net::IpAddr;

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};

/// The start of the fixed window of length `window` that contains `now`.
/// Windows line up with the Unix epoch, so every instance agrees on them.
pub fn window_start(now: DateTime<Utc>, window: Duration) -> DateTime<Utc> {
    let size = window.num_seconds().max(1);
    let start = now.timestamp().div_euclid(size) * size;
    DateTime::from_timestamp(start, 0).expect("in range")
}

/// Whole seconds until the window containing `now` ends, at least 1.
pub fn seconds_left(now: DateTime<Utc>, window: Duration) -> i64 {
    let end = window_start(now, window) + Duration::seconds(window.num_seconds().max(1));
    (end - now).num_seconds().max(1)
}

/// The network a client is counted under: the /24 for IPv4, the /48 for
/// IPv6 (about one site). IPv4-mapped IPv6 addresses count as IPv4.
pub fn network_prefix(ip: IpAddr) -> Vec<u8> {
    match ip.to_canonical() {
        IpAddr::V4(v4) => [&[4], &v4.octets()[..3]].concat(),
        IpAddr::V6(v6) => [&[6], &v6.octets()[..6]].concat(),
    }
}

/// A salted hash of the client's network. The raw address is never stored;
/// without the salt, the hash can't be walked back to a network.
pub fn network_key(salt: &[u8], ip: IpAddr) -> Vec<u8> {
    Sha256::new()
        .chain_update(salt)
        .chain_update([0])
        .chain_update(network_prefix(ip))
        .finalize()
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    #[test]
    fn windows_line_up_with_the_clock() {
        let minute = Duration::minutes(1);
        assert_eq!(
            window_start(at("2026-10-27T12:00:59Z"), minute),
            at("2026-10-27T12:00:00Z")
        );
        assert_eq!(seconds_left(at("2026-10-27T12:00:59Z"), minute), 1);
        assert_eq!(
            window_start(at("2026-10-27T12:34:56Z"), Duration::hours(1)),
            at("2026-10-27T12:00:00Z")
        );
    }

    #[test]
    fn a_network_is_a_slash_24_or_a_slash_48() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        assert_eq!(network_prefix(ip("203.0.113.9")), vec![4, 203, 0, 113]);
        assert_eq!(
            network_prefix(ip("::ffff:203.0.113.200")),
            network_prefix(ip("203.0.113.9"))
        );
        assert_eq!(
            network_prefix(ip("2001:db8:aa:1::1")),
            network_prefix(ip("2001:db8:aa:ffff::2"))
        );
        assert_ne!(
            network_prefix(ip("2001:db8:aa::1")),
            network_prefix(ip("2001:db8:ab::1"))
        );
    }
}

#[cfg(test)]
mod properties {
    use std::net::Ipv4Addr;

    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn now_falls_inside_its_window(secs in 0i64..4_000_000_000, window in 1i64..100_000) {
            let now = DateTime::from_timestamp(secs, 0).unwrap();
            let window = Duration::seconds(window);
            let start = window_start(now, window);
            prop_assert!(start <= now && now < start + window);
            prop_assert_eq!(start + window, now + Duration::seconds(seconds_left(now, window)));
        }

        #[test]
        fn the_key_depends_only_on_the_slash_24_and_the_salt(
            a in any::<[u8; 4]>(), b in any::<[u8; 4]>(),
            salt in any::<[u8; 16]>(), other in any::<[u8; 16]>(),
        ) {
            let key = |octets: [u8; 4], salt: &[u8]| network_key(salt, Ipv4Addr::from(octets).into());
            prop_assert_eq!(key(a, &salt) == key(b, &salt), a[..3] == b[..3]);
            prop_assume!(salt != other);
            prop_assert_ne!(key(a, &salt), key(a, &other));
        }
    }
}
