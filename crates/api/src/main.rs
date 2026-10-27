//! The `dorman-api` binary: reads config, connects, serves `dorman_api::app`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::http::HeaderName;
use clap::Parser;
use dorman_api::turnstile::CloudflareVerifier;
use dorman_api::{app, cookie_key, network_salt, AppState, Limits};
use dorman_core::{Clock, SystemClock, TurnstileVerifier};

#[derive(Parser)]
struct Args {
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,
    #[arg(long, env = "DORMAN_BIND", default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    /// At least 32 bytes of secret, used to sign the anonymous player cookie.
    #[arg(long, env = "DORMAN_COOKIE_SECRET")]
    cookie_secret: String,
    /// Mark cookies Secure (HTTPS only). Turn off only for local http dev.
    #[arg(long, env = "DORMAN_SECURE_COOKIES", default_value_t = true, action = clap::ArgAction::Set)]
    secure_cookies: bool,
    /// Where players reach the site, e.g. https://example.com. POSTs from
    /// any other origin are refused. Default: http://<bind>.
    #[arg(long, env = "DORMAN_PUBLIC_ORIGIN")]
    public_origin: Option<String>,
    /// Cloudflare Turnstile site key (public). For local dev, Cloudflare's
    /// always-pass test key is 1x00000000000000000000AA.
    #[arg(long, env = "DORMAN_TURNSTILE_SITE_KEY")]
    turnstile_site_key: String,
    /// Cloudflare Turnstile secret. For local dev, the always-pass test
    /// secret is 1x0000000000000000000000000000000AA.
    #[arg(long, env = "DORMAN_TURNSTILE_SECRET")]
    turnstile_secret: String,
    /// A header your proxy sets to the client's address (e.g. Fly-Client-IP
    /// or CF-Connecting-IP). Leave unset when clients connect directly;
    /// trusting a header no proxy overwrites lets anyone pick their address.
    #[arg(long, env = "DORMAN_CLIENT_IP_HEADER")]
    client_ip_header: Option<HeaderName>,
    #[arg(long, env = "DORMAN_VOTES_PER_MINUTE", default_value_t = Limits::default().votes_per_minute)]
    votes_per_minute: i32,
    /// New players allowed per hour from one /24 (IPv4) or /48 (IPv6).
    #[arg(long, env = "DORMAN_NEW_PLAYERS_PER_HOUR", default_value_t = Limits::default().new_players_per_hour)]
    new_players_per_hour: i32,
}

/// Cloudflare's always-pass and always-fail test secrets.
const TEST_SECRETS: &[&str] = &[
    "1x0000000000000000000000000000000AA",
    "2x0000000000000000000000000000000AA",
    "3x0000000000000000000000000000000AA",
];

/// The real clock, or with the `test-clock` feature (end-to-end tests only),
/// one read from the file named by `DORMAN_TEST_CLOCK_FILE`.
fn clock() -> Arc<dyn Clock> {
    #[cfg(feature = "test-clock")]
    if let Some(path) = std::env::var_os("DORMAN_TEST_CLOCK_FILE") {
        tracing::warn!("using the test clock from {path:?}; never do this in production");
        return Arc::new(dorman_api::test_clock::FileClock::new(path.into()));
    }
    Arc::new(SystemClock)
}

/// Cloudflare, or with the `test-clock` feature (end-to-end tests only), a
/// fake siteverify endpoint at `DORMAN_TEST_TURNSTILE_VERIFY_URL`.
fn turnstile(secret: String) -> Arc<dyn TurnstileVerifier> {
    #[cfg(feature = "test-clock")]
    if let Ok(url) = std::env::var("DORMAN_TEST_TURNSTILE_VERIFY_URL") {
        tracing::warn!("checking Turnstile tokens at {url}; never do this in production");
        return Arc::new(CloudflareVerifier::with_url(secret, url));
    }
    Arc::new(CloudflareVerifier::new(secret))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    anyhow::ensure!(
        args.cookie_secret.len() >= 32,
        "DORMAN_COOKIE_SECRET must be 32+ bytes"
    );
    anyhow::ensure!(
        args.votes_per_minute > 0 && args.new_players_per_hour > 0,
        "rate limits must be positive"
    );
    if TEST_SECRETS.contains(&args.turnstile_secret.as_str()) {
        tracing::warn!(
            "using a Cloudflare Turnstile test secret; fine locally, never in production"
        );
    }
    let public_origin = args
        .public_origin
        .unwrap_or_else(|| format!("http://{}", args.bind));
    let pool = dorman_db::connect(&args.database_url)
        .await
        .context("connecting to Postgres")?;
    dorman_db::migrate(&pool).await?;
    let state = AppState {
        pool,
        key: cookie_key(&args.cookie_secret),
        secure_cookies: args.secure_cookies,
        clock: clock(),
        turnstile: turnstile(args.turnstile_secret),
        turnstile_site_key: args.turnstile_site_key,
        public_origin: public_origin.trim_end_matches('/').to_string(),
        client_ip_header: args.client_ip_header,
        network_salt: network_salt(&args.cookie_secret),
        limits: Limits {
            votes_per_minute: args.votes_per_minute,
            new_players_per_hour: args.new_players_per_hour,
        },
    };
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    tracing::info!(
        "listening on http://{}, serving {}",
        listener.local_addr()?,
        state.public_origin
    );
    axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
