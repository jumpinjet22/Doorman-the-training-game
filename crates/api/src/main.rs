//! `dorman-api`: serves the daily game and takes votes.
//!
//! Pages are rendered here (askama) with a little vanilla JS; there is no
//! separate frontend build. Not yet in place, and needed before a public
//! launch: Cloudflare Turnstile on session start and votes, and rate limits.

mod views;

use std::net::SocketAddr;

use anyhow::Context;
use askama::Template;
use axum::extract::{FromRef, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, Key, SameSite, SignedCookieJar};
use chrono::Utc;
use clap::Parser;
use dorman_core::BatchItem;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use sqlx::PgPool;
use uuid::Uuid;

use views::{choice_label, PlayPage, PublicItem};

const PLAYER_COOKIE: &str = "player";
/// Anything slower than this is someone who walked away; clamp it.
const MAX_MS_TO_ANSWER: i32 = 10 * 60 * 1000;

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

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    key: Key,
    secure_cookies: bool,
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
    let day = Utc::now().date_naive();
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
    let day = Utc::now().date_naive();
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

fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(play))
        .route("/api/vote", post(vote))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state)
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
        // Key::from wants 64 bytes; stretch the secret to that.
        key: Key::from(&Sha512::digest(args.cookie_secret.as_bytes())),
        secure_cookies: args.secure_cookies,
    };
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    tracing::info!("listening on http://{}", args.bind);
    axum::serve(listener, app(state)).await?;
    Ok(())
}
