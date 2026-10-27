//! Taking a vote, by fetch from `play.js` (`POST /api/vote`, JSON) or as a
//! plain form post when the script didn't load (`POST /vote`). Both go
//! through `cast`: the item must be in today's set, the answer one of its
//! choices, the Turnstile token fresh and passed by Cloudflare, and the
//! player and their network inside their rate limits.

use askama::Template;
use axum::extract::{Form, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, SameSite, SignedCookieJar};
use chrono::Duration;
use dorman_core::limits::{network_key, seconds_left, window_start};
use dorman_core::turnstile::plausible_token;
use dorman_core::{BatchItem, Verdict};
use dorman_db::StoredItem;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::pages::finished_today;
use crate::security::ClientIp;
use crate::views::{choice_label, ErrorPage};
use crate::{known_player, AppError, AppState, PLAYER_COOKIE};

/// Anything slower than this is someone who walked away; clamp it.
const MAX_MS_TO_ANSWER: i32 = 10 * 60 * 1000;
/// Spent Turnstile tokens are remembered for longer than Cloudflare honours
/// them (five minutes).
const TOKEN_MEMORY: Duration = Duration::minutes(10);
/// Rate-limit rows, including the network hashes, are deleted after this.
const LIMIT_RETENTION: Duration = Duration::hours(24);

/// Why a vote wasn't taken.
#[derive(Debug)]
pub struct Refusal {
    pub status: StatusCode,
    pub message: &'static str,
    /// Seconds, for a 429.
    pub retry_after: Option<i64>,
}

impl Refusal {
    fn new(status: StatusCode, message: &'static str) -> Self {
        Self {
            status,
            message,
            retry_after: None,
        }
    }

    fn too_many(retry_after: i64, message: &'static str) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message,
            retry_after: Some(retry_after),
        }
    }

    fn headers(&self) -> Vec<(header::HeaderName, String)> {
        self.retry_after
            .map(|secs| (header::RETRY_AFTER, secs.to_string()))
            .into_iter()
            .collect()
    }

    /// Plain text, for `play.js`.
    fn text(self) -> Response {
        let headers = self.headers();
        (self.status, headers_map(headers), self.message).into_response()
    }

    /// A page with a way back, for a form post.
    fn page(self) -> Result<Response, AppError> {
        let headers = self.headers();
        let page = ErrorPage {
            message: self.message,
        };
        Ok((self.status, headers_map(headers), Html(page.render()?)).into_response())
    }
}

fn headers_map(headers: Vec<(header::HeaderName, String)>) -> axum::http::HeaderMap {
    headers
        .into_iter()
        .filter_map(|(name, value)| Some((name, value.parse().ok()?)))
        .collect()
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

/// The player behind this request, creating an anonymous one if needed and
/// this network hasn't made too many lately. Only call it once the request
/// has passed Turnstile.
async fn ensure_player(
    state: &AppState,
    jar: SignedCookieJar,
    ip: &ClientIp,
) -> Result<Result<(Uuid, SignedCookieJar), Refusal>, AppError> {
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
        return Ok(Err(Refusal::too_many(
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
/// passed by Cloudflare. Any other outcome is a refusal.
async fn human_check(
    state: &AppState,
    token: &str,
    ip: &ClientIp,
) -> Result<Option<Refusal>, AppError> {
    let refuse = |message| Ok(Some(Refusal::new(StatusCode::FORBIDDEN, message)));
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
            Ok(Some(Refusal::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "Couldn't run the human check just now. Try again.",
            )))
        }
    }
}

/// One vote, however it arrived.
pub struct Ballot {
    /// An Overtime answer rather than one for today's set.
    pub overtime: bool,
    pub item_id: Uuid,
    pub answer: String,
    pub ms_to_answer: i64,
    pub token: String,
}

/// A vote that was taken.
pub struct Taken {
    pub item: StoredItem,
    /// False if the player had already answered this item.
    pub recorded: bool,
    /// Items in today's set the player still hasn't answered (None in Overtime).
    pub remaining: Option<usize>,
}

async fn cast(
    state: &AppState,
    jar: SignedCookieJar,
    ip: ClientIp,
    ballot: Ballot,
) -> Result<(Result<Taken, Refusal>, SignedCookieJar), AppError> {
    let day = state.clock.today();
    let items = dorman_db::daily_set(&state.pool, day)
        .await?
        .unwrap_or_default();
    let overtime_item;
    let item = if ballot.overtime {
        // Only someone who's finished today plays Overtime, so there's no
        // new player here, and every Overtime player has trust from a daily.
        let player = known_player(state, &jar).await?;
        let finished = match player {
            Some(player) => finished_today(state, player, day).await?,
            None => false,
        };
        let Some(player) = player.filter(|_| finished) else {
            let refusal = Refusal::new(StatusCode::FORBIDDEN, "Finish today's game first.");
            return Ok((Err(refusal), jar));
        };
        overtime_item = dorman_db::overtime_item(&state.pool, player, day, ballot.item_id).await?;
        let Some(item) = &overtime_item else {
            let refusal = Refusal::new(StatusCode::NOT_FOUND, "That item isn't up for Overtime.");
            return Ok((Err(refusal), jar));
        };
        item
    } else {
        let Some(item) = items.iter().find(|item| item.id == ballot.item_id) else {
            let refusal = Refusal::new(StatusCode::NOT_FOUND, "That item isn't in today's set.");
            return Ok((Err(refusal), jar));
        };
        item
    };
    let valid = match &item.item {
        BatchItem::Intent(_) => item.options.contains(&ballot.answer),
        BatchItem::Pair(_) => item.item.accepts(&ballot.answer, &[]),
    };
    if !valid {
        let refusal = Refusal::new(StatusCode::UNPROCESSABLE_ENTITY, "Not one of the choices.");
        return Ok((Err(refusal), jar));
    }
    if let Some(refusal) = human_check(state, &ballot.token, &ip).await? {
        return Ok((Err(refusal), jar));
    }
    let (player, jar) = match ensure_player(state, jar, &ip).await? {
        Ok(found) => found,
        Err(refusal) => return Ok((Err(refusal), SignedCookieJar::new(state.key.clone()))),
    };
    let limit = state.limits.votes_per_minute;
    let key = player.as_bytes();
    if let Some(wait) = over_limit(state, "vote", key, Duration::minutes(1), limit).await? {
        return Ok((Err(Refusal::too_many(wait, "Slow down a little.")), jar));
    }
    let ms = ballot.ms_to_answer.clamp(0, MAX_MS_TO_ANSWER.into()) as i32;
    // Overtime answers belong to no day.
    let vote_day = (!ballot.overtime).then_some(day);
    let recorded =
        dorman_db::record_vote(&state.pool, player, item.id, vote_day, &ballot.answer, ms).await?;
    let remaining = if ballot.overtime {
        None
    } else {
        dorman_db::record_progress(&state.pool, player, day).await?;
        let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
        let answered = dorman_db::player_answers(&state.pool, player, &ids).await?;
        Some(items.len() - answered.len())
    };
    let taken = Taken {
        item: item.clone(),
        recorded,
        remaining,
    };
    Ok((Ok(taken), jar))
}

#[derive(Deserialize)]
pub struct VoteRequest {
    item_id: Uuid,
    answer: String,
    ms_to_answer: i32,
    #[serde(default)]
    turnstile_token: String,
    #[serde(default)]
    overtime: bool,
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
    /// Null in Overtime, which never runs out the same way.
    remaining: Option<usize>,
    split: Vec<SplitRow>,
}

/// `POST /api/vote`, from `play.js`: answers with the live split.
pub async fn vote_json(
    State(state): State<AppState>,
    jar: SignedCookieJar,
    ip: ClientIp,
    Json(request): Json<VoteRequest>,
) -> Result<Response, AppError> {
    let ballot = Ballot {
        overtime: request.overtime,
        item_id: request.item_id,
        answer: request.answer,
        ms_to_answer: request.ms_to_answer.into(),
        token: request.turnstile_token,
    };
    let (taken, jar) = match cast(&state, jar, ip, ballot).await? {
        (Ok(taken), jar) => (taken, jar),
        (Err(refusal), jar) => return Ok((jar, refusal.text()).into_response()),
    };
    let split = dorman_db::vote_split(&state.pool, taken.item.id)
        .await?
        .into_iter()
        .map(|(answer, count)| SplitRow {
            label: choice_label(&taken.item.item, &answer),
            answer,
            count,
        })
        .collect();
    let response = VoteResponse {
        recorded: taken.recorded,
        remaining: taken.remaining,
        split,
    };
    Ok((jar, Json(response)).into_response())
}

#[derive(Deserialize)]
pub struct VoteForm {
    item_id: Uuid,
    answer: String,
    /// When the server rendered the card, in Unix milliseconds. Without the
    /// script there's no other way to time the answer.
    shown_at: i64,
    #[serde(rename = "cf-turnstile-response", default)]
    turnstile_token: String,
    /// Present, as "1", on an Overtime card.
    overtime: Option<String>,
}

/// `POST /vote`: the same vote as a plain form post, for when `play.js`
/// didn't load. Redirects to the card's answered view.
pub async fn vote_form(
    State(state): State<AppState>,
    jar: SignedCookieJar,
    ip: ClientIp,
    Form(form): Form<VoteForm>,
) -> Result<Response, AppError> {
    let now = state.clock.now().timestamp_millis();
    let overtime = form.overtime.is_some();
    let ballot = Ballot {
        overtime,
        item_id: form.item_id,
        answer: form.answer,
        ms_to_answer: now.saturating_sub(form.shown_at),
        token: form.turnstile_token,
    };
    match cast(&state, jar, ip, ballot).await? {
        (Ok(taken), jar) => {
            let page = if overtime { "/overtime" } else { "/" };
            let to = format!("{page}?after={}", taken.item.id);
            Ok((jar, Redirect::to(&to)).into_response())
        }
        (Err(refusal), jar) => Ok((jar, refusal.page()?).into_response()),
    }
}
