//! The pages. Today's game shows one card at a time: the first one the
//! player hasn't answered, so a reload picks up where they left off. Once
//! they've answered everything it shows their results instead.

use std::collections::HashMap;

use askama::Template;
use axum::extract::{Form, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::SignedCookieJar;
use chrono::{Datelike, Days, NaiveDate};
use dorman_core::ranks::MIN_WEEKLY_SCORED;
use dorman_core::{
    day_number, leaderboard, next_reset, rank, score_answer, share_card, streak, Decision, Outcome,
    Rank, Record, Standing,
};
use dorman_db::{StoredItem, Verdict};
use serde::Deserialize;
use uuid::Uuid;

use crate::views::{
    bars, choice_label, until, AboutPage, Bar, BoardRow, CardPage, EmptyPage, ErrorPage,
    LeaderboardPage, Masthead, MessagePage, PublicItem, ResultRow, ResultsPage, RevealRow,
    YesterdayPage,
};
use crate::{known_player, AppError, AppState};

fn masthead(state: &AppState, day: NaiveDate) -> Masthead {
    Masthead {
        day: day.to_string(),
        number: day_number(state.launch_date, day),
    }
}

/// The live split of raw vote counts for one item, during the day.
async fn live_bars(
    state: &AppState,
    item: &StoredItem,
    mine: Option<&str>,
) -> Result<Vec<Bar>, AppError> {
    let split: Vec<(String, f64)> = dorman_db::vote_split(&state.pool, item.id)
        .await?
        .into_iter()
        .map(|(answer, count)| (answer, count as f64))
        .collect();
    Ok(bars(&split, |a| choice_label(&item.item, a), mine))
}

/// What the player has answered of `items` so far, by item.
async fn answers(
    state: &AppState,
    player: Option<Uuid>,
    items: &[StoredItem],
) -> Result<HashMap<Uuid, String>, AppError> {
    let Some(player) = player else {
        return Ok(HashMap::new());
    };
    let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
    Ok(dorman_db::player_answers(&state.pool, player, &ids)
        .await?
        .into_iter()
        .collect())
}

#[derive(Deserialize)]
pub struct PlayQuery {
    /// The card just answered by a plain form post, to show its split.
    after: Option<Uuid>,
}

/// `GET /`: today's next card, the card just answered, or the results.
pub async fn play(
    State(state): State<AppState>,
    jar: SignedCookieJar,
    Query(query): Query<PlayQuery>,
) -> Result<Response, AppError> {
    let day = state.clock.today();
    let Some(items) = dorman_db::daily_set(&state.pool, day)
        .await?
        .filter(|items| !items.is_empty())
    else {
        let now = state.clock.now();
        let reset = next_reset(now);
        let page = EmptyPage {
            masthead: masthead(&state, day),
            reset_at: reset.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            reset_in: until((reset - now).num_seconds()),
        };
        return Ok(Html(page.render()?).into_response());
    };
    let player = known_player(&state, &jar).await?;
    let answers = answers(&state, player, &items).await?;
    let remaining = items.len() - answers.len();

    let after = query.after.and_then(|id| {
        let index = items.iter().position(|item| item.id == id)?;
        Some((index, answers.get(&id)?.clone()))
    });
    if let Some((index, mine)) = after {
        let item = &items[index];
        let split = live_bars(&state, item, Some(&mine)).await?;
        return card(
            &state,
            day,
            &items,
            index,
            Some(mine),
            Some(split),
            remaining == 0,
        );
    }
    match items
        .iter()
        .position(|item| !answers.contains_key(&item.id))
    {
        Some(index) => card(&state, day, &items, index, None, None, remaining == 1),
        None => results_page(&state, day, &items, &answers, player).await,
    }
}

fn card(
    state: &AppState,
    day: NaiveDate,
    items: &[StoredItem],
    index: usize,
    answered: Option<String>,
    split: Option<Vec<Bar>>,
    last: bool,
) -> Result<Response, AppError> {
    let page = CardPage {
        masthead: masthead(state, day),
        position: index + 1,
        total: items.len(),
        item: PublicItem::from_stored(&items[index], answered),
        shown_at: state.clock.now().timestamp_millis(),
        turnstile_site_key: state.turnstile_site_key.clone(),
        split,
        last,
        overtime: None,
    };
    Ok(Html(page.render()?).into_response())
}

/// `GET /results`: today's results once every card is answered; until
/// then, back to the game.
pub async fn results(
    State(state): State<AppState>,
    jar: SignedCookieJar,
) -> Result<Response, AppError> {
    let day = state.clock.today();
    let items = dorman_db::daily_set(&state.pool, day)
        .await?
        .unwrap_or_default();
    let player = known_player(&state, &jar).await?;
    let answers = answers(&state, player, &items).await?;
    if items.is_empty() || answers.len() < items.len() {
        return Ok(Redirect::to("/").into_response());
    }
    results_page(&state, day, &items, &answers, player).await
}

fn decision(verdict: &Verdict) -> Decision {
    Decision {
        settled: (verdict.status == "settled")
            .then(|| verdict.label.clone())
            .flatten(),
        split: verdict.split.clone(),
    }
}

/// The squares for a day's card, in set order: pending until the scorer has
/// run after the day, then scored against the crowd's settled answer, the
/// same way for every item. Items in `hidden` stay pending.
async fn outcomes(
    state: &AppState,
    day: NaiveDate,
    items: &[StoredItem],
    answers: &HashMap<Uuid, String>,
    hidden: &[Uuid],
) -> Result<Vec<Outcome>, AppError> {
    let out = dorman_db::results_out(&state.pool, day).await?;
    let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
    let verdicts = dorman_db::verdicts(&state.pool, &ids).await?;
    Ok(items
        .iter()
        .map(|item| match answers.get(&item.id) {
            Some(_) if hidden.contains(&item.id) => Outcome::Pending,
            Some(answer) => {
                score_answer(answer, verdicts.get(&item.id).map(decision).as_ref(), out)
            }
            None => Outcome::Pending,
        })
        .collect())
}

async fn player_streak(
    state: &AppState,
    player: Option<Uuid>,
    today: NaiveDate,
) -> Result<u32, AppError> {
    let Some(player) = player else { return Ok(0) };
    let days = dorman_db::completed_days(&state.pool, player).await?;
    Ok(streak(&days, today))
}

async fn results_page(
    state: &AppState,
    day: NaiveDate,
    items: &[StoredItem],
    answers: &HashMap<Uuid, String>,
    player: Option<Uuid>,
) -> Result<Response, AppError> {
    // Every card is answered, maybe on earlier days if items came round
    // again, so the day counts towards the streak even without a vote today.
    if let Some(player) = player {
        dorman_db::record_progress(&state.pool, player, day).await?;
    }
    let outcomes = outcomes(state, day, items, answers, &[]).await?;
    let streak = player_streak(state, player, day).await?;
    let mut rows = Vec::new();
    for item in items {
        let mine = answers.get(&item.id).cloned();
        let bars = live_bars(state, item, mine.as_deref()).await?;
        rows.push(ResultRow {
            mine: mine
                .as_deref()
                .map(|a| choice_label(&item.item, a))
                .unwrap_or_default(),
            item: PublicItem::from_stored(item, mine),
            bars,
        });
    }
    let (rank, handle) = match player {
        Some(player) => (
            Some(player_rank(state, player).await?.name()),
            dorman_db::handle(&state.pool, player).await?,
        ),
        None => (None, None),
    };
    let page = ResultsPage {
        masthead: masthead(state, day),
        rank,
        handle,
        card: share_card(
            day_number(state.launch_date, day).unwrap_or(0),
            &outcomes,
            streak,
        ),
        streak,
        pending: outcomes.contains(&Outcome::Pending),
        rows,
    };
    Ok(Html(page.render()?).into_response())
}

/// What became of an item, in words.
fn verdict_words(verdict: &Verdict, label: &dyn Fn(&str) -> String) -> String {
    match (verdict.status.as_str(), &verdict.label) {
        ("settled", Some(answer)) => format!("The crowd settled on: {}.", label(answer)),
        ("disputed", _) => "Too close to call. A person will take a look.".into(),
        _ => "Not enough votes yet. It may come back for another round.".into(),
    }
}

/// `GET /yesterday`: yesterday's items once the night's scoring has run,
/// every one shown the same way whether or not its answer was known in
/// advance, so the page can't be used to pick those out. An item that's back
/// in today's set is left out (its split would tip off today's answers), and
/// how the crowd answered is shown only for items the viewer answered
/// themselves: they'll never be asked those again, so the page can't teach
/// anyone answers to use later.
pub async fn yesterday(
    State(state): State<AppState>,
    jar: SignedCookieJar,
) -> Result<Response, AppError> {
    let today = state.clock.today();
    let day = today - Days::new(1);
    let items = dorman_db::daily_set(&state.pool, day)
        .await?
        .unwrap_or_default();
    let mut page = YesterdayPage {
        masthead: masthead(&state, day),
        played_day: !items.is_empty(),
        out: dorman_db::results_out(&state.pool, day).await?,
        rows: Vec::new(),
        card: None,
    };
    if page.played_day && page.out {
        let player = known_player(&state, &jar).await?;
        let answers = answers(&state, player, &items).await?;
        let back_today: Vec<Uuid> = dorman_db::daily_set(&state.pool, today)
            .await?
            .unwrap_or_default()
            .iter()
            .map(|item| item.id)
            .collect();
        if !answers.is_empty() {
            let outcomes = outcomes(&state, day, &items, &answers, &back_today).await?;
            let number = day_number(state.launch_date, day).unwrap_or(0);
            page.card = Some(share_card(number, &outcomes, 0));
        }
        let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
        let verdicts = dorman_db::verdicts(&state.pool, &ids).await?;
        for item in items.iter().filter(|item| !back_today.contains(&item.id)) {
            let Some(mine) = answers.get(&item.id).cloned() else {
                page.rows.push(RevealRow {
                    verdict: "You didn't answer this one, so how the crowd answered stays \
                              hidden."
                        .into(),
                    bars: Vec::new(),
                    item: PublicItem::from_stored(item, None),
                });
                continue;
            };
            let public = PublicItem::from_stored(item, Some(mine.clone()));
            // Only labels for choices the item actually offered: never a
            // raw key, whatever the scorer wrote.
            let labels: HashMap<String, String> = public
                .choices
                .iter()
                .map(|c| (c.value.clone(), c.label.clone()))
                .collect();
            let label = |answer: &str| {
                labels
                    .get(answer)
                    .cloned()
                    .unwrap_or_else(|| "Something else".into())
            };
            let (verdict, bars) = match verdicts.get(&item.id) {
                Some(verdict) => (
                    verdict_words(verdict, &label),
                    bars(&verdict.split, label, Some(&mine)),
                ),
                None => ("No votes were counted for this one.".into(), Vec::new()),
            };
            page.rows.push(RevealRow {
                verdict,
                bars,
                item: public,
            });
        }
    }
    Ok(Html(page.render()?).into_response())
}

async fn player_rank(state: &AppState, player: Uuid) -> Result<Rank, AppError> {
    let (days, scored, right) = dorman_db::player_record(&state.pool, player).await?;
    let count = |n: i64| u32::try_from(n).unwrap_or(u32::MAX);
    Ok(rank(Record {
        days: count(days),
        scored: count(scored),
        right: count(right),
    }))
}

/// `GET /overtime`: endless play once today's set is done, from items
/// already shown in past days, lightest first. Back to `/` otherwise.
pub async fn overtime(
    State(state): State<AppState>,
    jar: SignedCookieJar,
    Query(query): Query<PlayQuery>,
) -> Result<Response, AppError> {
    let today = state.clock.today();
    let Some(player) = known_player(&state, &jar).await? else {
        return Ok(Redirect::to("/").into_response());
    };
    if !finished_today(&state, player, today).await? {
        return Ok(Redirect::to("/").into_response());
    }
    let answered = dorman_db::overtime_answered(&state.pool, player).await?;
    let masthead = masthead(&state, today);
    if let Some(id) = query.after {
        let mine = dorman_db::player_answers(&state.pool, player, &[id]).await?;
        if let (Some((_, mine)), Some(item)) =
            (mine.first(), dorman_db::item(&state.pool, id).await?)
        {
            let split = live_bars(&state, &item, Some(mine)).await?;
            return overtime_card(
                &state,
                masthead,
                &item,
                Some(mine.clone()),
                Some(split),
                answered,
            );
        }
    }
    match dorman_db::overtime_next(&state.pool, player, today).await? {
        Some(item) => overtime_card(&state, masthead, &item, None, None, answered),
        None => {
            let page = MessagePage {
                heading: "Overtime",
                message: "You've answered everything there is for now. New items arrive every day.",
                link: ("/", "Back to today's results"),
            };
            Ok(Html(page.render()?).into_response())
        }
    }
}

fn overtime_card(
    state: &AppState,
    masthead: Masthead,
    item: &StoredItem,
    answered: Option<String>,
    split: Option<Vec<Bar>>,
    count: i64,
) -> Result<Response, AppError> {
    let page = CardPage {
        masthead,
        position: 0,
        total: 0,
        item: PublicItem::from_stored(item, answered),
        shown_at: state.clock.now().timestamp_millis(),
        turnstile_site_key: state.turnstile_site_key.clone(),
        split,
        last: false,
        overtime: Some(count),
    };
    Ok(Html(page.render()?).into_response())
}

/// Whether the player has answered every item in today's set.
pub async fn finished_today(
    state: &AppState,
    player: Uuid,
    today: NaiveDate,
) -> Result<bool, AppError> {
    let items = dorman_db::daily_set(&state.pool, today)
        .await?
        .unwrap_or_default();
    if items.is_empty() {
        return Ok(false);
    }
    let answers = answers(state, Some(player), &items).await?;
    Ok(answers.len() == items.len())
}

/// `GET /leaderboard`: this week (Monday to today, UTC) by accuracy against
/// settled answers, players with a handle only.
pub async fn leaderboard_page(State(state): State<AppState>) -> Result<Response, AppError> {
    let today = state.clock.today();
    let monday = today - Days::new(u64::from(today.weekday().num_days_from_monday()));
    let count = |n: i64| u32::try_from(n).unwrap_or(u32::MAX);
    let standings = dorman_db::weekly_standings(&state.pool, monday, today)
        .await?
        .into_iter()
        .map(|(handle, scored, right, flagged)| Standing {
            handle,
            scored: count(scored),
            right: count(right),
            flagged,
        })
        .collect();
    let rows = leaderboard(standings)
        .into_iter()
        .enumerate()
        .map(|(n, s)| BoardRow {
            place: n + 1,
            percent: (100.0 * f64::from(s.right) / f64::from(s.scored)).round() as u32,
            handle: s.handle,
            scored: s.scored,
        })
        .collect();
    let page = LeaderboardPage {
        week_of: monday.to_string(),
        minimum: MIN_WEEKLY_SCORED,
        rows,
    };
    Ok(Html(page.render()?).into_response())
}

/// A handle shown on the leaderboard: 3 to 20 letters, digits, `_` or `-`,
/// starting with a letter or digit.
pub fn valid_handle(handle: &str) -> bool {
    (3..=20).contains(&handle.len())
        && handle.starts_with(|c: char| c.is_ascii_alphanumeric())
        && handle
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[derive(Deserialize)]
pub struct HandleForm {
    handle: String,
}

/// `POST /handle`: picks the player's name for the leaderboard.
pub async fn set_handle(
    State(state): State<AppState>,
    jar: SignedCookieJar,
    Form(form): Form<HandleForm>,
) -> Result<Response, AppError> {
    let refuse = |status: StatusCode, message: &'static str| -> Result<Response, AppError> {
        Ok((status, Html(ErrorPage { message }.render()?)).into_response())
    };
    let Some(player) = known_player(&state, &jar).await? else {
        return refuse(
            StatusCode::FORBIDDEN,
            "Play a game first, then pick a name.",
        );
    };
    let handle = form.handle.trim();
    if !valid_handle(handle) {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "A name is 3 to 20 letters, digits, dashes or underscores.",
        );
    }
    if !dorman_db::set_handle(&state.pool, player, handle).await? {
        return refuse(
            StatusCode::CONFLICT,
            "Someone already has that name. Try another.",
        );
    }
    Ok(Redirect::to("/leaderboard").into_response())
}

pub async fn about() -> Result<Response, AppError> {
    Ok(Html(AboutPage.render()?).into_response())
}
