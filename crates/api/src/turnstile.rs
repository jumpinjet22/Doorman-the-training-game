//! The real Turnstile verifier: asks Cloudflare's siteverify endpoint about
//! each token. Anything short of a clear "yes" from Cloudflare fails, and
//! anything that stops Cloudflare answering comes back `Unavailable`, which
//! the routes also treat as a failure.
//!
//! With `expecting`, a pass must also name our host and the `vote` action,
//! so a token minted on another site or for another action with the same
//! site key is rejected.

use std::net::IpAddr;
use std::time::Duration;

use dorman_core::turnstile::BoxFuture;
use dorman_core::{TurnstileVerifier, Verdict};
use serde::Deserialize;

pub const SITEVERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
/// A vote waits at most this long for Cloudflare before giving up.
const TIMEOUT: Duration = Duration::from_secs(5);
/// The widget's `data-action` on the vote form.
pub const ACTION: &str = "vote";

pub struct CloudflareVerifier {
    client: reqwest::Client,
    secret: String,
    url: String,
    /// The host a token must have been solved on; None skips the hostname
    /// and action checks (Cloudflare's test secrets answer for any host).
    hostname: Option<String>,
}

impl CloudflareVerifier {
    pub fn new(secret: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("an HTTP client");
        Self {
            client,
            secret,
            url: SITEVERIFY_URL.to_string(),
            hostname: None,
        }
    }

    /// Talks to `url` instead of Cloudflare. Tests only.
    pub fn with_url(secret: String, url: String) -> Self {
        Self::new(secret).at(url)
    }

    /// Talks to `url` instead of Cloudflare. Tests only.
    pub fn at(mut self, url: String) -> Self {
        self.url = url;
        self
    }

    /// Only passes tokens solved on `hostname` (no port) for [`ACTION`].
    pub fn expecting(mut self, hostname: impl Into<String>) -> Self {
        self.hostname = Some(hostname.into());
        self
    }

    async fn check(&self, token: &str, remote_ip: Option<IpAddr>) -> Verdict {
        let mut form = vec![("secret", self.secret.clone()), ("response", token.into())];
        if let Some(ip) = remote_ip {
            form.push(("remoteip", ip.to_string()));
        }
        let response = match self.client.post(&self.url).form(&form).send().await {
            Ok(response) => response,
            Err(err) => return Verdict::Unavailable(format!("siteverify unreachable: {err}")),
        };
        if !response.status().is_success() {
            return Verdict::Unavailable(format!("siteverify answered {}", response.status()));
        }
        let body: SiteVerify = match response.json().await {
            Ok(body) => body,
            Err(err) => return Verdict::Unavailable(format!("siteverify sent nonsense: {err}")),
        };
        let codes = body.error_codes.join(",");
        if body.success {
            match &self.hostname {
                Some(host) if body.hostname.as_deref() != Some(host) => {
                    Verdict::Rejected(format!("hostname {:?}", body.hostname))
                }
                Some(_) if body.action.as_deref() != Some(ACTION) => {
                    Verdict::Rejected(format!("action {:?}", body.action))
                }
                _ => Verdict::Passed,
            }
        } else if body
            .error_codes
            .iter()
            .any(|code| OUR_FAULT.contains(&code.as_str()))
        {
            // Cloudflare's trouble or a misconfigured secret, not the player's.
            Verdict::Unavailable(format!("siteverify error: {codes}"))
        } else {
            Verdict::Rejected(codes)
        }
    }
}

/// Error codes that say nothing about the token itself.
const OUR_FAULT: &[&str] = &[
    "internal-error",
    "missing-input-secret",
    "invalid-input-secret",
];

#[derive(Deserialize)]
struct SiteVerify {
    success: bool,
    #[serde(default, rename = "error-codes")]
    error_codes: Vec<String>,
    hostname: Option<String>,
    action: Option<String>,
}

impl TurnstileVerifier for CloudflareVerifier {
    fn verify<'a>(&'a self, token: &'a str, remote_ip: Option<IpAddr>) -> BoxFuture<'a, Verdict> {
        Box::pin(self.check(token, remote_ip))
    }
}
