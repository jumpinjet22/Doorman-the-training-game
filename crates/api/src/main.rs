//! The `dorman-api` binary: reads config, connects, serves `dorman_api::app`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use dorman_api::{app, cookie_key, AppState};
use dorman_core::{Clock, SystemClock};

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
    let pool = dorman_db::connect(&args.database_url)
        .await
        .context("connecting to Postgres")?;
    dorman_db::migrate(&pool).await?;
    let state = AppState {
        pool,
        key: cookie_key(&args.cookie_secret),
        secure_cookies: args.secure_cookies,
        clock: clock(),
    };
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    tracing::info!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app(state)).await?;
    Ok(())
}
