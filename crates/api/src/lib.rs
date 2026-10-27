//! `dorman-api`: serves the daily game and takes votes.
//!
//! Pages are rendered here (askama) with a little vanilla JS
//! (`static/play.js`); there is no separate frontend build.
//!
//! Abuse resistance: every vote carries a Cloudflare Turnstile token, checked
//! before anything is written; a player is only created by a vote, so by a
//! verified one. Votes per player and new players per network are rate
//! limited in Postgres. See `security` for headers and the Origin check.

pub mod config;
pub mod pages;
pub mod security;
#[cfg(feature = "test-clock")]
pub mod test_clock;
pub mod turnstile;
pub mod views;
pub mod voting;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, FromRef};
use axum::http::{header, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{middleware, Router};
use axum_extra::extract::cookie::{Key, SignedCookieJar};
use chrono::NaiveDate;
use dorman_core::{Clock, TurnstileVerifier};
use sha2::{Digest, Sha256, Sha512};
use sqlx::PgPool;
use uuid::Uuid;

const PLAYER_COOKIE: &str = "player";
/// No request needs more than this. A vote is ~100 bytes plus a Turnstile
/// token of at most 2 KiB.
pub const MAX_BODY_BYTES: usize = 8 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub key: Key,
    pub secure_cookies: bool,
    pub clock: Arc<dyn Clock>,
    pub turnstile: Arc<dyn TurnstileVerifier>,
    /// Public, rendered into the page for the Turnstile widget.
    pub turnstile_site_key: String,
    /// Where the page is served from, e.g. `https://example.com`. POSTs must
    /// carry exactly this `Origin`.
    pub public_origin: String,
    /// A header set by a trusted proxy with the client's address. None means
    /// use the TCP peer address.
    pub client_ip_header: Option<HeaderName>,
    /// Secret salt for hashing networks.
    pub network_salt: Vec<u8>,
    pub limits: Limits,
    /// Day 1 of the game, for day numbers.
    pub launch_date: NaiveDate,
}

/// How much one player, or one network, may do.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub votes_per_minute: i32,
    /// New players from one /24 (IPv4) or /48 (IPv6) per hour.
    pub new_players_per_hour: i32,
}

impl Default for Limits {
    fn default() -> Self {
        // A quick human answers a card every few seconds; a whole household
        // or office behind one address starts playing in the same hour.
        Self {
            votes_per_minute: 30,
            new_players_per_hour: 20,
        }
    }
}

// The cookie key and the network salt both come from DORMAN_COOKIE_SECRET
// but are independent: different hash functions over different inputs (the
// salt's is prefixed with its own label), so neither reveals the other or the
// secret. Changing either derivation logs every player out or resets every
// network's rate-limit window, so the tests pin both to known answers.

/// The salt for network hashes, derived from the cookie secret so there's
/// one fewer secret to manage.
pub fn network_salt(secret: &str) -> Vec<u8> {
    Sha256::new()
        .chain_update("dorman network salt")
        .chain_update([0])
        .chain_update(secret)
        .finalize()
        .to_vec()
}

/// The cookie-signing key for a secret of 32+ bytes. `Key::from` wants 64
/// bytes, so the secret is stretched with SHA-512.
pub fn cookie_key(secret: &str) -> Key {
    Key::from(&Sha512::digest(secret.as_bytes()))
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Key {
        state.key.clone()
    }
}

pub struct AppError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        AppError(err.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!("{:#}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong.").into_response()
    }
}

/// The player a signed cookie names, if it's genuine and they still exist.
/// A missing, unsigned or tampered cookie is no one.
async fn known_player(state: &AppState, jar: &SignedCookieJar) -> Result<Option<Uuid>, AppError> {
    let Some(id) = jar
        .get(PLAYER_COOKIE)
        .and_then(|c| Uuid::parse_str(c.value()).ok())
    else {
        return Ok(None);
    };
    Ok(dorman_db::player_exists(&state.pool, id)
        .await?
        .then_some(id))
}

fn static_file(content_type: &'static str, body: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        body,
    )
}

async fn play_js() -> impl IntoResponse {
    static_file(
        "text/javascript; charset=utf-8",
        include_str!("../static/play.js"),
    )
}

async fn play_css() -> impl IntoResponse {
    static_file(
        "text/css; charset=utf-8",
        include_str!("../static/play.css"),
    )
}

/// Brand assets are immutable files; cache them for a day.
fn asset(content_type: &'static str, body: &'static [u8]) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        body,
    )
}

macro_rules! asset_route {
    ($router:expr, $path:literal, $type:literal, $file:literal) => {
        $router.route(
            $path,
            get(|| async { asset($type, include_bytes!(concat!("../static/", $file))) }),
        )
    };
}

pub fn app(state: AppState) -> Router {
    let router = Router::new()
        .route("/", get(pages::play))
        .route("/results", get(pages::results))
        .route("/yesterday", get(pages::yesterday))
        .route("/about", get(pages::about))
        .route("/overtime", get(pages::overtime))
        .route("/leaderboard", get(pages::leaderboard_page))
        .route("/handle", post(pages::set_handle))
        .route("/vote", post(voting::vote_form))
        .route("/api/vote", post(voting::vote_json))
        .route("/static/play.js", get(play_js))
        .route("/static/play.css", get(play_css))
        .route("/healthz", get(|| async { "ok" }));
    let router = asset_route!(
        router,
        "/static/brand/favicon.svg",
        "image/svg+xml",
        "brand/favicon.svg"
    );
    let router = asset_route!(
        router,
        "/static/brand/knock-mark-light.svg",
        "image/svg+xml",
        "brand/knock-mark-light.svg"
    );
    let router = asset_route!(
        router,
        "/static/brand/knock-mark-dark.svg",
        "image/svg+xml",
        "brand/knock-mark-dark.svg"
    );
    let router = asset_route!(
        router,
        "/static/fonts/big-shoulders-display-700.woff2",
        "font/woff2",
        "fonts/big-shoulders-display-latin-700-normal.woff2"
    );
    let router = asset_route!(
        router,
        "/static/fonts/big-shoulders-display-900.woff2",
        "font/woff2",
        "fonts/big-shoulders-display-latin-900-normal.woff2"
    );
    let router = asset_route!(
        router,
        "/static/fonts/ibm-plex-sans-400.woff2",
        "font/woff2",
        "fonts/ibm-plex-sans-latin-400-normal.woff2"
    );
    let router = asset_route!(
        router,
        "/static/fonts/ibm-plex-sans-500.woff2",
        "font/woff2",
        "fonts/ibm-plex-sans-latin-500-normal.woff2"
    );
    let router = asset_route!(
        router,
        "/static/fonts/ibm-plex-sans-600.woff2",
        "font/woff2",
        "fonts/ibm-plex-sans-latin-600-normal.woff2"
    );
    router
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security::same_origin,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security::headers,
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "9f2c41d07be35a86e1f0c4b9d27a6e53";
    /// SHA-512 of SECRET, as on day one.
    const COOKIE_KEY_HEX: &str = "a42bd7c4d00c3a7fa69e3963fd5e4f78583a0b367efb8141754b6528a48052e26624b37373ecb6a1dbd9ffd525cbb95bdc1aadc808d173a4152c112862e5d249";
    /// SHA-256 of "dorman network salt", a zero byte and SECRET.
    const NETWORK_SALT_HEX: &str =
        "dc98982e461fb483d48c46c584c4fb645295ba0c165e87efa7c3b52f909c6106";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn the_derivations_are_independent_and_never_change() {
        let key = cookie_key(SECRET);
        let salt = network_salt(SECRET);
        assert_eq!(hex(key.master()), COOKIE_KEY_HEX);
        assert_eq!(hex(&salt), NETWORK_SALT_HEX);
        assert!(!key.master().windows(salt.len()).any(|w| w == salt));
        assert_ne!(hex(cookie_key("other").master()), COOKIE_KEY_HEX);
        assert_ne!(network_salt("other"), salt);
    }
}
