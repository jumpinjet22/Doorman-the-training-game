//! `dorman-api`: serves the daily game and takes votes.
//!
//! Pages are rendered here (askama) with a little vanilla JS
//! (`static/play.js`); there is no separate frontend build. Not yet in place,
//! and needed before a public launch: Cloudflare Turnstile on session start and
//! votes, and rate limits.

#[cfg(feature = "test-clock")]
pub mod test_clock;
pub mod views;

use std::sync::Arc;

use askama::Template;
use axum::extract::{FromRef, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, Key, SameSite, SignedCookieJar};
use dorman_core::{BatchItem, Clock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use sqlx::PgPool;
use uuid::Uuid;

use views::{choice_label, PlayPage, PublicItem};

const PLAYER_COOKIE: &str = "player";
/// Anything slower than this is someone who walked away; clamp it.
const MAX_MS_TO_ANSWER: i32 = 10 * 60 * 1000;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub key: Key,
    pub secure_cookies: bool,
    pub clock: Arc<dyn Clock>,
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

/// The player behind this request, creating an anonymous one if needed.
async fn ensure_player(
    state: &AppState,
    jar: SignedCookieJar,
) -> Result<(Uuid, SignedCookieJar), AppError> {
    let existing = jar
        .get(PLAYER_COOKIE)
        .and_then(|c| Uuid::parse_str(c.value()).ok());
    if let Some(id) = existing {
        if dorman_db::player_exists(&state.pool, id).await? {
            return Ok((id, jar));
        }
    }
    let id = dorman_db::create_player(&state.pool).await?;
    let cookie = Cookie::build((PLAYER_COOKIE, id.to_string()))
        .path("/")
        .http_only(true)
        .secure(state.secure_cookies)
        .same_site(SameSite::Lax)
        .permanent()
        .build();
    Ok((id, jar.add(cookie)))
}

async fn play(
    State(state): State<AppState>,
    jar: SignedCookieJar,
) -> Result<impl IntoResponse, AppError> {
    let day = state.clock.today();
    let (player, jar) = ensure_player(&state, jar).await?;
    let items = dorman_db::daily_set(&state.pool, day)
        .await?
        .unwrap_or_default();
    let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
    let answers = dorman_db::player_answers(&state.pool, player, &ids).await?;
    let page = PlayPage {
        day: day.to_string(),
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
    Ok((jar, Html(page.render()?)))
}

#[derive(Deserialize)]
struct VoteRequest {
    item_id: Uuid,
    answer: String,
    ms_to_answer: i32,
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
    let (player, jar) = ensure_player(&state, jar).await?;
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

async fn play_js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        include_str!("../static/play.js"),
    )
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(play))
        .route("/api/vote", post(vote))
        .route("/static/play.js", get(play_js))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state)
}
