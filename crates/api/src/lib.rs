//! `dorman-api`: serves the daily game and takes votes.
//!
//! Pages are rendered here (askama) with a little vanilla JS
//! (`static/play.js`); there is no separate frontend build.
//!
//! Abuse resistance: every vote carries a Cloudflare Turnstile token, checked
//! before anything is written; a player is only created by a vote, so by a
//! verified one. Votes per player and new players per network are rate
//! limited in Postgres. See `security` for headers and the Origin check.

pub mod security;
#[cfg(feature = "test-clock")]
pub mod test_clock;
pub mod turnstile;
pub mod views;

use std::sync::Arc;

use askama::Template;
use axum::extract::{DefaultBodyLimit, FromRef, State};
use axum::http::{header, HeaderName, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{middleware, Json, Router};
use axum_extra::extract::cookie::{Cookie, Key, SameSite, SignedCookieJar};
use chrono::Duration;
use dorman_core::limits::{network_key, seconds_left, window_start};
use dorman_core::turnstile::plausible_token;
use dorman_core::{BatchItem, Clock, TurnstileVerifier, Verdict};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use sqlx::PgPool;
use uuid::Uuid;

use security::ClientIp;
use views::{choice_label, PlayPage, PublicItem};

const PLAYER_COOKIE: &str = "player";
/// Anything slower than this is someone who walked away; clamp it.
const MAX_MS_TO_ANSWER: i32 = 10 * 60 * 1000;
/// No request needs more than this. A vote is ~100 bytes plus a Turnstile
/// token of at most 2 KiB.
pub const MAX_BODY_BYTES: usize = 8 * 1024;
/// Spent Turnstile tokens are remembered for longer than Cloudflare honours
/// them (five minutes).
const TOKEN_MEMORY: Duration = Duration::minutes(10);
/// Rate-limit rows, including the network hashes, are deleted after this.
const LIMIT_RETENTION: Duration = Duration::hours(24);

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

struct AppError(anyhow::Error);

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

fn too_many(retry_after: i64, message: &'static str) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after.to_string())],
        message,
    )
        .into_response()
}

/// Counts a hit against `key` and returns Retry-After seconds if the window
/// is already over `limit`.
async fn over_limit(
    state: &AppState,
    scope: &str,
    key: &[u8],
    window: Duration,
    limit: i32,
) -> Result<Option<i64>, AppError> {
    let now = state.clock.now();
    let hits =
        dorman_db::hit_rate_limit(&state.pool, scope, key, window_start(now, window)).await?;
    Ok((hits > limit).then(|| seconds_left(now, window)))
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

/// The player behind this request, creating an anonymous one if needed and
/// this network hasn't made too many lately. Only call it once the request
/// has passed Turnstile.
async fn ensure_player(
    state: &AppState,
    jar: SignedCookieJar,
    ip: ClientIp,
) -> Result<Result<(Uuid, SignedCookieJar), Response>, AppError> {
    if let Some(id) = known_player(state, &jar).await? {
        return Ok(Ok((id, jar)));
    }
    let now = state.clock.now();
    dorman_db::purge_rate_limits(&state.pool, now - LIMIT_RETENTION).await?;
    // With no address at all, everyone shares one bucket: that fails closed.
    let network = match ip.0 {
        Some(ip) => network_key(&state.network_salt, ip),
        None => {
            tracing::warn!("no client address; check DORMAN_CLIENT_IP_HEADER");
            b"unknown".to_vec()
        }
    };
    let limit = state.limits.new_players_per_hour;
    if let Some(wait) = over_limit(state, "new_player", &network, Duration::hours(1), limit).await?
    {
        return Ok(Err(too_many(
            wait,
            "Too many new players from your network. Try later.",
        )));
    }
    let id = dorman_db::create_player(&state.pool).await?;
    let cookie = Cookie::build((PLAYER_COOKIE, id.to_string()))
        .path("/")
        .http_only(true)
        .secure(state.secure_cookies)
        .same_site(SameSite::Lax)
        .permanent()
        .build();
    Ok(Ok((id, jar.add(cookie))))
}

/// Checks a request's Turnstile token: present, never seen before, and
/// passed by Cloudflare. Any other outcome is an error response.
async fn human_check(
    state: &AppState,
    token: &str,
    ip: &ClientIp,
) -> Result<Option<Response>, AppError> {
    let refuse = |message| Ok(Some((StatusCode::FORBIDDEN, message).into_response()));
    if !plausible_token(token) {
        return refuse("Please complete the human check.");
    }
    let now = state.clock.now();
    let hash = Sha256::digest(token.as_bytes());
    if !dorman_db::claim_token(&state.pool, &hash, now, now - TOKEN_MEMORY).await? {
        return refuse("That human check was already used.");
    }
    match state.turnstile.verify(token, ip.0).await {
        Verdict::Passed => Ok(None),
        Verdict::Rejected(reason) => {
            tracing::info!("turnstile rejected a token: {reason}");
            refuse("The human check failed. Try again.")
        }
        Verdict::Unavailable(reason) => {
            tracing::warn!("turnstile unavailable, refusing the request: {reason}");
            Ok(Some(
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Couldn't run the human check just now. Try again.",
                )
                    .into_response(),
            ))
        }
    }
}

/// Today's set. Never creates a player: that only happens on a verified vote.
async fn play(
    State(state): State<AppState>,
    jar: SignedCookieJar,
) -> Result<impl IntoResponse, AppError> {
    let day = state.clock.today();
    let player = known_player(&state, &jar).await?;
    let items = dorman_db::daily_set(&state.pool, day)
        .await?
        .unwrap_or_default();
    let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
    let answers = match player {
        Some(player) => dorman_db::player_answers(&state.pool, player, &ids).await?,
        None => Vec::new(),
    };
    let page = PlayPage {
        day: day.to_string(),
        turnstile_site_key: state.turnstile_site_key.clone(),
        items: items
            .iter()
            .map(|item| {
                let answered = answers
                    .iter()
                    .find(|(id, _)| *id == item.id)
                    .map(|(_, a)| a.clone());
                PublicItem::from_stored(item, answered)
            })
            .collect(),
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
struct VoteRequest {
    item_id: Uuid,
    answer: String,
    ms_to_answer: i32,
    #[serde(default)]
    turnstile_token: String,
}

#[derive(Serialize)]
struct SplitRow {
    answer: String,
    label: String,
    count: i64,
}

#[derive(Serialize)]
struct VoteResponse {
    recorded: bool,
    split: Vec<SplitRow>,
}

async fn vote(
    State(state): State<AppState>,
    jar: SignedCookieJar,
    ip: ClientIp,
    Json(request): Json<VoteRequest>,
) -> Result<Response, AppError> {
    let day = state.clock.today();
    let items = dorman_db::daily_set(&state.pool, day)
        .await?
        .unwrap_or_default();
    let Some(item) = items.iter().find(|item| item.id == request.item_id) else {
        return Ok((StatusCode::NOT_FOUND, "That item isn't in today's set.").into_response());
    };
    let valid = match &item.item {
        BatchItem::Intent(_) => item.options.contains(&request.answer),
        BatchItem::Pair(_) => item.item.accepts(&request.answer, &[]),
    };
    if !valid {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, "Not one of the choices.").into_response());
    }
    if let Some(refused) = human_check(&state, &request.turnstile_token, &ip).await? {
        return Ok(refused);
    }
    let (player, jar) = match ensure_player(&state, jar, ip).await? {
        Ok(found) => found,
        Err(refused) => return Ok(refused),
    };
    let limit = state.limits.votes_per_minute;
    if let Some(wait) = over_limit(
        &state,
        "vote",
        player.as_bytes(),
        Duration::minutes(1),
        limit,
    )
    .await?
    {
        return Ok((jar, too_many(wait, "Slow down a little.")).into_response());
    }
    let ms = request.ms_to_answer.clamp(0, MAX_MS_TO_ANSWER);
    let recorded =
        dorman_db::record_vote(&state.pool, player, item.id, Some(day), &request.answer, ms)
            .await?;
    let split = dorman_db::vote_split(&state.pool, item.id)
        .await?
        .into_iter()
        .map(|(answer, count)| SplitRow {
            label: choice_label(&item.item, &answer),
            answer,
            count,
        })
        .collect();
    Ok((jar, Json(VoteResponse { recorded, split })).into_response())
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

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(play))
        .route("/api/vote", post(vote))
        .route("/static/play.js", get(play_js))
        .route("/static/play.css", get(play_css))
        .route("/healthz", get(|| async { "ok" }))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security::same_origin,
        ))
        .layer(middleware::from_fn(security::headers))
        .with_state(state)
}
