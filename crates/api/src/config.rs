//! Checks on the server's configuration, run once at startup. Each refuses a
//! setting that would quietly weaken the game in production; `main` turns
//! the error into a refusal to start.

use std::path::Path;

use anyhow::{bail, ensure, Context};
use reqwest::Url;

/// Cloudflare's always-pass, always-fail and force-challenge test secrets.
pub const TEST_SECRETS: &[&str] = &[
    "1x0000000000000000000000000000000AA",
    "2x0000000000000000000000000000000AA",
    "3x0000000000000000000000000000000AA",
];
/// Cloudflare's test site keys (visible pass, visible fail, invisible pass,
/// invisible fail, force challenge).
pub const TEST_SITE_KEYS: &[&str] = &[
    "1x00000000000000000000AA",
    "2x00000000000000000000AB",
    "1x00000000000000000000BB",
    "2x00000000000000000000BB",
    "3x00000000000000000000FF",
];

/// The shortest cookie secret accepted.
pub const MIN_SECRET_BYTES: usize = 32;
/// A secret with fewer different characters than this is a pattern, not
/// random: 32 random hex digits almost always use 13 or more of the 16.
const MIN_DISTINCT_CHARS: usize = 8;
/// No run of this many bytes may appear twice. In random hex or base64 a
/// repeat this long is vanishingly unlikely; in `secretsecret...` it's certain.
const REPEAT_WINDOW: usize = 8;

/// The public origin, parsed: `http` or `https`, a host, and nothing after it.
pub fn public_origin(origin: &str) -> anyhow::Result<Url> {
    let url = Url::parse(origin).context("DORMAN_PUBLIC_ORIGIN is not a URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "DORMAN_PUBLIC_ORIGIN must be http(s)://host[:port]"
    );
    ensure!(
        url.path() == "/" && url.query().is_none() && url.fragment().is_none(),
        "DORMAN_PUBLIC_ORIGIN must be an origin, with no path"
    );
    Ok(url)
}

/// Whether the origin is this machine: only then are dev shortcuts allowed.
pub fn is_local(origin: &Url) -> bool {
    matches!(origin.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

/// Cloudflare's test keys pass (or fail) every token, so they mean there is
/// no human check at all. Refused for anything but local development: a
/// plain-http origin on this machine. Returns whether test keys are in use,
/// for the caller to warn about.
pub fn check_turnstile_keys(site_key: &str, secret: &str, origin: &Url) -> anyhow::Result<bool> {
    let test = TEST_SECRETS.contains(&secret) || TEST_SITE_KEYS.contains(&site_key);
    if test && !(origin.scheme() == "http" && is_local(origin)) {
        bail!(
            "Cloudflare Turnstile test keys accept every visitor; they are only \
             allowed with an http://localhost, http://127.0.0.1 or http://[::1] \
             DORMAN_PUBLIC_ORIGIN"
        );
    }
    Ok(test)
}

/// The cookie secret, from `DORMAN_COOKIE_SECRET` or the file named by
/// `DORMAN_COOKIE_SECRET_FILE` (trailing whitespace trimmed), never both.
pub fn cookie_secret(value: Option<String>, file: Option<&Path>) -> anyhow::Result<String> {
    let secret = match (value, file) {
        (Some(_), Some(_)) => {
            bail!("set DORMAN_COOKIE_SECRET or DORMAN_COOKIE_SECRET_FILE, not both")
        }
        (Some(value), None) => value,
        (None, Some(path)) => std::fs::read_to_string(path)
            .with_context(|| format!("reading DORMAN_COOKIE_SECRET_FILE {}", path.display()))?
            .trim_end()
            .to_string(),
        (None, None) => bail!("DORMAN_COOKIE_SECRET or DORMAN_COOKIE_SECRET_FILE is required"),
    };
    check_cookie_secret(&secret)?;
    Ok(secret)
}

/// Refuses a secret that is short or visibly not random. Never echoes it.
pub fn check_cookie_secret(secret: &str) -> anyhow::Result<()> {
    let bytes = secret.as_bytes();
    ensure!(
        bytes.len() >= MIN_SECRET_BYTES,
        "the cookie secret must be {MIN_SECRET_BYTES}+ bytes; try `openssl rand -hex 32`"
    );
    let mut seen = [false; 256];
    for &b in bytes {
        seen[usize::from(b)] = true;
    }
    let distinct = seen.iter().filter(|&&s| s).count();
    let mut windows: Vec<&[u8]> = bytes.windows(REPEAT_WINDOW).collect();
    windows.sort_unstable();
    let repeats = windows.windows(2).any(|pair| pair[0] == pair[1]);
    ensure!(
        distinct >= MIN_DISTINCT_CHARS && !repeats,
        "the cookie secret looks repetitive, not random; try `openssl rand -hex 32`"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: &str = "http://127.0.0.1:8080";
    const PROD: &str = "https://example.com";
    const PASS_KEY: &str = "1x00000000000000000000AA";
    const PASS_SECRET: &str = "1x0000000000000000000000000000000AA";

    fn origin(s: &str) -> Url {
        public_origin(s).unwrap()
    }

    #[test]
    fn origins_must_be_bare_http_or_https() {
        for good in [LOCAL, PROD, "http://localhost", "http://[::1]:3000/"] {
            assert!(public_origin(good).is_ok(), "{good}");
        }
        for bad in [
            "example.com",
            "ftp://example.com",
            "https://example.com/game",
            "https://example.com/?x=1",
            "https://example.com/#top",
            "file:///tmp",
        ] {
            assert!(public_origin(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_this_machine_is_local() {
        for local in ["http://localhost:8080", LOCAL, "http://[::1]"] {
            assert!(is_local(&origin(local)), "{local}");
        }
        for remote in [
            PROD,
            "http://0.0.0.0:8080",
            "http://192.168.1.5",
            "http://localhost.example.com",
            "http://127.0.0.1.nip.io",
        ] {
            assert!(!is_local(&origin(remote)), "{remote}");
        }
    }

    #[test]
    fn test_keys_only_for_local_http_development() {
        let real = ("0x4AAAAAAAreal-site-key", "0x4AAAAAAAreal-secret");
        // Locally, over http: allowed, and reported.
        for local in [LOCAL, "http://localhost", "http://[::1]:3000"] {
            assert!(check_turnstile_keys(PASS_KEY, PASS_SECRET, &origin(local)).unwrap());
        }
        assert!(check_turnstile_keys(PASS_KEY, real.1, &origin(LOCAL)).unwrap());
        assert!(check_turnstile_keys(real.0, PASS_SECRET, &origin(LOCAL)).unwrap());
        // A test secret or a test site key on a real origin, or over https
        // (a production setting), is refused.
        for (key, secret) in [
            (PASS_KEY, PASS_SECRET),
            (PASS_KEY, real.1),
            (real.0, PASS_SECRET),
        ] {
            for url in [PROD, "http://example.com", "https://localhost"] {
                assert!(
                    check_turnstile_keys(key, secret, &origin(url)).is_err(),
                    "{key} {secret} {url}"
                );
            }
        }
        for secret in TEST_SECRETS {
            assert!(check_turnstile_keys(real.0, secret, &origin(PROD)).is_err());
        }
        for key in TEST_SITE_KEYS {
            assert!(check_turnstile_keys(key, real.1, &origin(PROD)).is_err());
        }
        // Real keys are fine anywhere.
        assert!(!check_turnstile_keys(real.0, real.1, &origin(PROD)).unwrap());
        assert!(!check_turnstile_keys(real.0, real.1, &origin(LOCAL)).unwrap());
    }

    #[test]
    fn random_secrets_pass() {
        for secret in [
            // `openssl rand -hex 16`, `-hex 32` and `-base64 32`.
            "9f2c41d07be35a86e1f0c4b9d27a6e53",
            "4b1e9d7c02a85f36e9c1d40b7a28f5e3c6d91b04e7f2a85c3d60b9e17f4a2c58",
            "q7Vd2kP9xJmZ4rT1bW8nY3cH6sL0fA5gE2uK9oI4pRw=",
            "correct horse battery staple, moonlit kettle drum",
        ] {
            assert!(check_cookie_secret(secret).is_ok(), "{secret}");
        }
    }

    #[test]
    fn short_or_repetitive_secrets_fail_without_echoing_them() {
        for secret in [
            "too-short",
            "9f2c41d07be35a86e1f0c4b9d27a6e5",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "abababababababababababababababababab",
            "0123456701234567012345670123456701234567",
            "test-secret-test-secret-test-secret",
            "my-cookie-secret-is-great-my-cookie-secret",
        ] {
            let err = check_cookie_secret(secret).unwrap_err().to_string();
            assert!(!err.contains(secret), "{err}");
        }
    }

    #[test]
    fn the_secret_comes_from_the_variable_or_a_file_but_not_both() {
        let good = "9f2c41d07be35a86e1f0c4b9d27a6e53";
        let dir = std::env::temp_dir().join(format!("dorman-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("secret");
        std::fs::write(&file, format!("{good}\n")).unwrap();

        assert_eq!(cookie_secret(Some(good.into()), None).unwrap(), good);
        assert_eq!(cookie_secret(None, Some(&file)).unwrap(), good);
        assert!(cookie_secret(Some(good.into()), Some(&file)).is_err());
        assert!(cookie_secret(None, None).is_err());
        assert!(cookie_secret(None, Some(&dir.join("missing"))).is_err());
        assert!(cookie_secret(Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()), None).is_err());
        std::fs::write(&file, "short\n").unwrap();
        assert!(cookie_secret(None, Some(&file)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
