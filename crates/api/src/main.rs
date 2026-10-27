//! The `dorman-api` binary: reads config, connects, serves `dorman_api::app`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::http::HeaderName;
use clap::Parser;
use dorman_api::config::{self, TEST_SECRETS};
use dorman_api::turnstile::CloudflareVerifier;
use dorman_api::{app, cookie_key, network_salt, AppState, Limits};
use dorman_core::{Clock, SystemClock, TurnstileVerifier};

#[derive(Parser)]
struct Args {
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,
    #[arg(long, env = "DORMAN_BIND", default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    /// At least 32 random bytes, used to sign the anonymous player cookie
    /// and salt network hashes. Env only: hidden from --help, since a flag
    /// shows in the process list. Prefer DORMAN_COOKIE_SECRET_FILE.
    #[arg(
        long,
        env = "DORMAN_COOKIE_SECRET",
        hide = true,
        hide_env_values = true
    )]
    cookie_secret: Option<String>,
    /// A file holding the cookie secret (e.g. a mounted secret).
    #[arg(long, env = "DORMAN_COOKIE_SECRET_FILE")]
    cookie_secret_file: Option<std::path::PathBuf>,
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
    #[arg(long, env = "DORMAN_TURNSTILE_SECRET", hide_env_values = true)]
    turnstile_secret: String,
    /// A header your proxy sets to the client's address (e.g. Fly-Client-IP
    /// or CF-Connecting-IP). Leave unset when clients connect directly;
    /// trusting a header no proxy overwrites lets anyone pick their address.
    #[arg(long, env = "DORMAN_CLIENT_IP_HEADER")]
    client_ip_header: Option<HeaderName>,
    /// Day 1 of the game (YYYY-MM-DD). Day numbers count from here.
    #[arg(long, env = "DORMAN_LAUNCH_DATE")]
    launch_date: chrono::NaiveDate,
    #[arg(long, env = "DORMAN_VOTES_PER_MINUTE", default_value_t = Limits::default().votes_per_minute)]
    votes_per_minute: i32,
    /// New players allowed per hour from one /24 (IPv4) or /48 (IPv6).
    #[arg(long, env = "DORMAN_NEW_PLAYERS_PER_HOUR", default_value_t = Limits::default().new_players_per_hour)]
    new_players_per_hour: i32,
}

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
/// fake siteverify endpoint at `DORMAN_TEST_TURNSTILE_VERIFY_URL`. Tokens
/// must name `hostname`, when given.
fn turnstile(secret: String, hostname: Option<String>) -> Arc<dyn TurnstileVerifier> {
    let verifier = CloudflareVerifier::new(secret);
    #[cfg(feature = "test-clock")]
    let verifier = match std::env::var("DORMAN_TEST_TURNSTILE_VERIFY_URL") {
        Ok(url) => {
            tracing::warn!("checking Turnstile tokens at {url}; never do this in production");
            verifier.at(url)
        }
        Err(_) => verifier,
    };
    Arc::new(match hostname {
        Some(host) => verifier.expecting(host),
        None => verifier,
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let cookie_secret =
        config::cookie_secret(args.cookie_secret, args.cookie_secret_file.as_deref())?;
    anyhow::ensure!(
        args.votes_per_minute > 0 && args.new_players_per_hour > 0,
        "rate limits must be positive"
    );
    let public_origin = args
        .public_origin
        .unwrap_or_else(|| format!("http://{}", args.bind));
    let origin = config::public_origin(&public_origin)?;
    config::check_secure_cookies(&origin, args.secure_cookies)?;
    let test_keys = config::check_turnstile_keys(
        &args.turnstile_site_key,
        &args.turnstile_secret,
        &origin,
        args.secure_cookies,
    )?;
    if test_keys {
        tracing::warn!("using Cloudflare Turnstile test keys; fine locally, never in production");
    }
    // A test secret's answers name no real host, so only check real ones.
    let token_host = (!TEST_SECRETS.contains(&args.turnstile_secret.as_str()))
        .then(|| origin.host_str().unwrap_or_default().to_string());
    let pool = dorman_db::connect(&args.database_url)
        .await
        .context("connecting to Postgres")?;
    dorman_db::migrate(&pool).await?;
    let state = AppState {
        pool,
        key: cookie_key(&cookie_secret),
        secure_cookies: args.secure_cookies,
        clock: clock(),
        turnstile: turnstile(args.turnstile_secret, token_host),
        turnstile_site_key: args.turnstile_site_key,
        public_origin: public_origin.trim_end_matches('/').to_string(),
        client_ip_header: args.client_ip_header,
        network_salt: network_salt(&cookie_secret),
        launch_date: args.launch_date,
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
