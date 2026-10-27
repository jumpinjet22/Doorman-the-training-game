//! Cloudflare Turnstile, as the game sees it: a token goes in, a verdict
//! comes out. The real verifier (an HTTP call to Cloudflare) lives in
//! `dorman-api`; tests use the stub in `dorman-testkit`.

use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;

/// Cloudflare caps tokens at 2048 characters; anything longer is forged.
pub const MAX_TOKEN_LEN: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    /// Cloudflare looked at the token and said no: forged, expired, reused.
    Rejected(String),
    /// Couldn't get an answer. Callers must treat this as a failure.
    Unavailable(String),
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait TurnstileVerifier: Send + Sync {
    fn verify<'a>(&'a self, token: &'a str, remote_ip: Option<IpAddr>) -> BoxFuture<'a, Verdict>;
}

/// Whether a token is worth sending to Cloudflare at all.
pub fn plausible_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_TOKEN_LEN && token.is_ascii()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_oversized_and_non_ascii_tokens_are_implausible() {
        assert!(plausible_token("0.abc-DEF_123"));
        assert!(plausible_token(&"a".repeat(MAX_TOKEN_LEN)));
        assert!(!plausible_token(""));
        assert!(!plausible_token(&"a".repeat(MAX_TOKEN_LEN + 1)));
        assert!(!plausible_token("tökén"));
    }
}
