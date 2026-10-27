//! Postgres access. Queries use sqlx's runtime-checked API rather than the
//! `query!` macros so the workspace builds without a live database; switch to
//! the macros (and `cargo sqlx prepare`) once CI has a Postgres service.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use dorman_core::{intent_options, BatchItem, INTENT_OPTION_COUNT};
use serde_json::Value;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use uuid::Uuid;

pub use sqlx::Error;

pub async fn connect(database_url: &str) -> Result<PgPool, Error> {
    PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
}

pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

/// One item as stored: the exported batch item plus the options shown.
#[derive(Debug, Clone)]
pub struct StoredItem {
    pub id: Uuid,
    pub item_key: String,
    pub item: BatchItem,
    /// Choices for an intent item (empty for pair items).
    pub options: Vec<String>,
}

fn stored_item(row: &sqlx::postgres::PgRow) -> Result<StoredItem, Error> {
    let mut payload: Value = row.try_get("payload")?;
    let options = match payload
        .as_object_mut()
        .and_then(|obj| obj.remove("options"))
    {
        Some(value) => serde_json::from_value(value).map_err(|e| Error::Decode(e.into()))?,
        None => Vec::new(),
    };
    let item = serde_json::from_value(payload).map_err(|e| Error::Decode(e.into()))?;
    Ok(StoredItem {
        id: row.try_get("id")?,
        item_key: row.try_get("item_key")?,
        item,
        options,
    })
}

/// Stores one batch item. Returns false when its item_key is already loaded.
/// A batch seen for the first time starts out pending review.
pub async fn insert_item(
    pool: &PgPool,
    batch: &str,
    intents: &[String],
    item: &BatchItem,
) -> Result<bool, Error> {
    let mut payload = serde_json::to_value(item).map_err(|e| Error::Encode(e.into()))?;
    if let BatchItem::Intent(intent) = item {
        let options = intent_options(
            &intent.item_key,
            &intent.intended_intent,
            intent.gold_answer.as_deref(),
            intents,
            INTENT_OPTION_COUNT,
        );
        payload["options"] = serde_json::json!(options);
    }
    sqlx::query("INSERT INTO batches (name) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(batch)
        .execute(pool)
        .await?;
    let result = sqlx::query(
        "INSERT INTO items (id, item_key, mode, payload, gold_answer, batch)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (item_key) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(item.item_key())
    .bind(item.mode())
    .bind(payload)
    .bind(item.gold_answer())
    .bind(batch)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Candidate item_keys for a new daily set.
#[derive(Debug, Default)]
pub struct DailyPools {
    /// Never in a daily set, no gold answer.
    pub new: Vec<String>,
    pub gold: Vec<String>,
    /// In `before`'s set and still open or disputed, least settled first.
    pub carry_over: Vec<String>,
}

fn item_keys(rows: &[sqlx::postgres::PgRow]) -> Result<Vec<String>, Error> {
    rows.iter().map(|row| row.try_get("item_key")).collect()
}

/// SQL true when item `i` comes from a batch a person has approved. A macro
/// rather than a `const` so queries can `concat!` it into one `&'static str`:
/// sqlx refuses SQL assembled at runtime, and this way none is.
macro_rules! approved {
    () => {
        "EXISTS (SELECT 1 FROM batches b WHERE b.name = i.batch AND b.status = 'approved')"
    };
}

/// How many daily sets a gold item is shown in before it's retired. Gold
/// that keeps coming back is gold players can learn to recognise; five
/// showings lets an item measure five fresh waves of players, and with three
/// gold a day a pool of 30 lasts 50 days before it needs topping up.
pub const GOLD_SHOWINGS: i64 = 5;

/// Only items from approved batches are candidates, and gold retires after
/// `GOLD_SHOWINGS` days in a set.
pub async fn daily_pools(pool: &PgPool, before: NaiveDate) -> Result<DailyPools, Error> {
    daily_pools_retiring(pool, before, GOLD_SHOWINGS).await
}

/// `daily_pools`, retiring gold after `gold_showings` sets up to `before`.
pub async fn daily_pools_retiring(
    pool: &PgPool,
    before: NaiveDate,
    gold_showings: i64,
) -> Result<DailyPools, Error> {
    let new = sqlx::query(concat!(
        "SELECT item_key FROM items i
         WHERE gold_answer IS NULL AND ",
        approved!(),
        "
           AND NOT EXISTS (SELECT 1 FROM daily_sets d WHERE i.id = ANY (d.item_ids))
         ORDER BY item_key"
    ))
    .fetch_all(pool)
    .await?;
    let gold = sqlx::query(concat!(
        "SELECT item_key FROM items i
         WHERE gold_answer IS NOT NULL AND ",
        approved!(),
        "
           AND (SELECT count(*) FROM daily_sets d
                WHERE i.id = ANY (d.item_ids) AND d.day <= $1) < $2
         ORDER BY item_key"
    ))
    .bind(before)
    .bind(gold_showings)
    .fetch_all(pool)
    .await?;
    let carry_over = sqlx::query(concat!(
        "SELECT i.item_key FROM daily_sets d
         JOIN items i ON i.id = ANY (d.item_ids)
         LEFT JOIN consensus c ON c.item_id = i.id
         WHERE d.day = $1 AND i.gold_answer IS NULL AND ",
        approved!(),
        "
           AND coalesce(c.status, 'open') <> 'settled'
         ORDER BY coalesce(c.share, 0), i.item_key"
    ))
    .bind(before)
    .fetch_all(pool)
    .await?;
    Ok(DailyPools {
        new: item_keys(&new)?,
        gold: item_keys(&gold)?,
        carry_over: item_keys(&carry_over)?,
    })
}

/// Publishes the set for `day`, replacing any earlier one for that day.
/// Items from batches that aren't approved are left out.
pub async fn set_daily(pool: &PgPool, day: NaiveDate, item_keys: &[String]) -> Result<(), Error> {
    sqlx::query(concat!(
        "INSERT INTO daily_sets (day, item_ids)
         SELECT $1, coalesce(array_agg(i.id ORDER BY k.ord), '{}')
         FROM unnest($2::text[]) WITH ORDINALITY AS k(key, ord)
         JOIN items i ON i.item_key = k.key AND ",
        approved!(),
        "
         ON CONFLICT (day) DO UPDATE SET item_ids = excluded.item_ids, published_at = now()"
    ))
    .bind(day)
    .bind(item_keys)
    .execute(pool)
    .await?;
    Ok(())
}

/// The day's items in order, or None if no set was published for it.
pub async fn daily_set(pool: &PgPool, day: NaiveDate) -> Result<Option<Vec<StoredItem>>, Error> {
    let exists = sqlx::query("SELECT 1 FROM daily_sets WHERE day = $1")
        .bind(day)
        .fetch_optional(pool)
        .await?;
    if exists.is_none() {
        return Ok(None);
    }
    let rows = sqlx::query(
        "SELECT i.id, i.item_key, i.payload
         FROM daily_sets d, unnest(d.item_ids) WITH ORDINALITY AS k(id, ord)
         JOIN items i ON i.id = k.id
         WHERE d.day = $1
         ORDER BY k.ord",
    )
    .bind(day)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(stored_item)
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

pub async fn create_player(pool: &PgPool) -> Result<Uuid, Error> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO players (id) VALUES ($1)")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(id)
}

pub async fn player_exists(pool: &PgPool, id: Uuid) -> Result<bool, Error> {
    let row = sqlx::query("SELECT 1 FROM players WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

/// Records one answer. Returns false if the player already answered this item.
pub async fn record_vote(
    pool: &PgPool,
    player_id: Uuid,
    item_id: Uuid,
    day: Option<NaiveDate>,
    answer: &str,
    ms_to_answer: i32,
) -> Result<bool, Error> {
    let result = sqlx::query(
        "INSERT INTO votes (player_id, item_id, day, answer, ms_to_answer)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (player_id, item_id) DO NOTHING",
    )
    .bind(player_id)
    .bind(item_id)
    .bind(day)
    .bind(answer)
    .bind(ms_to_answer)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// `record_vote`, plus how sure the player said they were
/// (`unsure`, `fairly` or `certain`).
pub async fn record_vote_with_confidence(
    pool: &PgPool,
    player_id: Uuid,
    item_id: Uuid,
    day: Option<NaiveDate>,
    answer: &str,
    ms_to_answer: i32,
    confidence: Option<&str>,
) -> Result<bool, Error> {
    let result = sqlx::query(
        "INSERT INTO votes (player_id, item_id, day, answer, ms_to_answer, confidence)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (player_id, item_id) DO NOTHING",
    )
    .bind(player_id)
    .bind(item_id)
    .bind(day)
    .bind(answer)
    .bind(ms_to_answer)
    .bind(confidence)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// How sure the player said they were, for the items where they said.
pub async fn player_confidences(
    pool: &PgPool,
    player_id: Uuid,
    item_ids: &[Uuid],
) -> Result<Vec<(Uuid, String)>, Error> {
    let rows = sqlx::query(
        "SELECT item_id, confidence FROM votes
         WHERE player_id = $1 AND item_id = ANY ($2) AND confidence IS NOT NULL",
    )
    .bind(player_id)
    .bind(item_ids)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| Ok((row.try_get("item_id")?, row.try_get("confidence")?)))
        .collect()
}

/// The player's answers so far, keyed by item.
pub async fn player_answers(
    pool: &PgPool,
    player_id: Uuid,
    item_ids: &[Uuid],
) -> Result<Vec<(Uuid, String)>, Error> {
    let rows = sqlx::query(
        "SELECT item_id, answer FROM votes WHERE player_id = $1 AND item_id = ANY ($2)",
    )
    .bind(player_id)
    .bind(item_ids)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| Ok((row.try_get("item_id")?, row.try_get("answer")?)))
        .collect()
}

/// Raw (unweighted) vote counts for one item, most popular first. Shown to a
/// player only after they've answered it.
pub async fn vote_split(pool: &PgPool, item_id: Uuid) -> Result<Vec<(String, i64)>, Error> {
    let rows = sqlx::query(
        "SELECT answer, count(*) AS n FROM votes WHERE item_id = $1
         GROUP BY answer ORDER BY n DESC, answer",
    )
    .bind(item_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| Ok((row.try_get("answer")?, row.try_get("n")?)))
        .collect()
}

// -- batch review -------------------------------------------------------------

/// `pending` or `approved`, or None for a batch that was never loaded.
pub async fn batch_status(pool: &PgPool, batch: &str) -> Result<Option<String>, Error> {
    sqlx::query_scalar("SELECT status FROM batches WHERE name = $1")
        .bind(batch)
        .fetch_optional(pool)
        .await
}

/// Every item in a batch, in item_key order, for a person to skim.
pub async fn batch_items(pool: &PgPool, batch: &str) -> Result<Vec<StoredItem>, Error> {
    let rows =
        sqlx::query("SELECT id, item_key, payload FROM items WHERE batch = $1 ORDER BY item_key")
            .bind(batch)
            .fetch_all(pool)
            .await?;
    rows.iter().map(stored_item).collect()
}

/// Makes a pending batch eligible for daily sets. Returns false when it was
/// already approved or doesn't exist.
pub async fn approve_batch(pool: &PgPool, batch: &str, now: DateTime<Utc>) -> Result<bool, Error> {
    let result = sqlx::query(
        "UPDATE batches SET status = 'approved', approved_at = $2
         WHERE name = $1 AND status = 'pending'",
    )
    .bind(batch)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

// -- abuse limits -------------------------------------------------------------

/// Counts one hit against `key` in the window starting at `window_start`
/// and returns the window's total so far, this hit included. Atomic, so
/// concurrent requests on any number of instances are all counted.
pub async fn hit_rate_limit(
    pool: &PgPool,
    scope: &str,
    key: &[u8],
    window_start: DateTime<Utc>,
) -> Result<i32, Error> {
    sqlx::query_scalar(
        "INSERT INTO rate_limits (scope, key, window_start, hits) VALUES ($1, $2, $3, 1)
         ON CONFLICT (scope, key, window_start)
         DO UPDATE SET hits = rate_limits.hits + 1
         RETURNING hits",
    )
    .bind(scope)
    .bind(key)
    .bind(window_start)
    .fetch_one(pool)
    .await
}

/// Deletes rate-limit windows that started before `before`.
pub async fn purge_rate_limits(pool: &PgPool, before: DateTime<Utc>) -> Result<u64, Error> {
    let result = sqlx::query("DELETE FROM rate_limits WHERE window_start < $1")
        .bind(before)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Marks a Turnstile token (by its hash) as spent. Returns false if it was
/// spent already. Hashes used before `forget_before` are dropped first;
/// Cloudflare refuses tokens that old anyway.
pub async fn claim_token(
    pool: &PgPool,
    token_hash: &[u8],
    now: DateTime<Utc>,
    forget_before: DateTime<Utc>,
) -> Result<bool, Error> {
    sqlx::query("DELETE FROM turnstile_tokens WHERE used_at < $1")
        .bind(forget_before)
        .execute(pool)
        .await?;
    let result = sqlx::query(
        "INSERT INTO turnstile_tokens (token_hash, used_at) VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(token_hash)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

// -- progress, streaks and results ---------------------------------------------

/// Notes that the player played `day`, and whether they've now answered
/// every item in its set. Returns whether the day is complete. A day once
/// complete stays complete.
pub async fn record_progress(
    pool: &PgPool,
    player_id: Uuid,
    day: NaiveDate,
) -> Result<bool, Error> {
    sqlx::query_scalar(
        "INSERT INTO player_days (player_id, day, completed)
         SELECT $1, d.day,
                (SELECT count(*) FROM votes v
                 WHERE v.player_id = $1 AND v.item_id = ANY (d.item_ids)) >= cardinality(d.item_ids)
         FROM daily_sets d WHERE d.day = $2
         ON CONFLICT (player_id, day)
         DO UPDATE SET completed = player_days.completed OR excluded.completed
         RETURNING completed",
    )
    .bind(player_id)
    .bind(day)
    .fetch_optional(pool)
    .await
    .map(|completed| completed.unwrap_or(false))
}

/// Every day the player completed, oldest first.
pub async fn completed_days(pool: &PgPool, player_id: Uuid) -> Result<Vec<NaiveDate>, Error> {
    sqlx::query_scalar(
        "SELECT day FROM player_days WHERE player_id = $1 AND completed ORDER BY day",
    )
    .bind(player_id)
    .fetch_all(pool)
    .await
}

/// Whether the scorer has run since `day` ended, so its results are final
/// for the night. A run is logged under the day it ran.
pub async fn results_out(pool: &PgPool, day: NaiveDate) -> Result<bool, Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM scoring_runs WHERE day > $1)")
        .bind(day)
        .fetch_one(pool)
        .await
}

/// What the scorer decided about one item.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// `open`, `settled` or `disputed`.
    pub status: String,
    pub label: Option<String>,
    /// Answer -> trust behind it, heaviest first.
    pub split: Vec<(String, f64)>,
}

/// The scorer's verdicts for these items. Items it never decided on (gold
/// items, or anything not yet scored) are left out.
pub async fn verdicts(pool: &PgPool, item_ids: &[Uuid]) -> Result<HashMap<Uuid, Verdict>, Error> {
    let rows =
        sqlx::query("SELECT item_id, status, label, split FROM consensus WHERE item_id = ANY ($1)")
            .bind(item_ids)
            .fetch_all(pool)
            .await?;
    rows.iter()
        .map(|row| {
            let split: HashMap<String, f64> = serde_json::from_value(row.try_get("split")?)
                .map_err(|e| Error::Decode(e.into()))?;
            let mut split: Vec<(String, f64)> = split.into_iter().collect();
            split.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            Ok((
                row.try_get("item_id")?,
                Verdict {
                    status: row.try_get("status")?,
                    label: row.try_get("label")?,
                    split,
                },
            ))
        })
        .collect()
}

// -- overtime -----------------------------------------------------------------

/// SQL true when item `i` can be served to player $1 in Overtime on day $2:
/// approved, already shown in an earlier daily set (so Overtime never
/// previews tomorrow's items), not in today's set, and not yet answered by
/// them. Gold is served like anything else, so Overtime can't sort it out.
macro_rules! overtime {
    () => {
        concat!(
            approved!(),
            "
    AND EXISTS (SELECT 1 FROM daily_sets d WHERE i.id = ANY (d.item_ids) AND d.day < $2)
    AND NOT EXISTS (SELECT 1 FROM daily_sets d WHERE i.id = ANY (d.item_ids) AND d.day = $2)
    AND NOT EXISTS (SELECT 1 FROM votes v WHERE v.item_id = i.id AND v.player_id = $1)"
        )
    };
}

/// The next Overtime item for a player: the one with the least total weight
/// behind it, ties broken in an order of the player's own.
pub async fn overtime_next(
    pool: &PgPool,
    player_id: Uuid,
    today: NaiveDate,
) -> Result<Option<StoredItem>, Error> {
    let row = sqlx::query(concat!(
        "SELECT i.id, i.item_key, i.payload FROM items i
         LEFT JOIN consensus c ON c.item_id = i.id
         WHERE ",
        overtime!(),
        "
         ORDER BY coalesce(c.total_weight, 0), md5(i.id::text || $1::text)
         LIMIT 1"
    ))
    .bind(player_id)
    .bind(today)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(stored_item).transpose()
}

/// The item, if the player may answer it in Overtime today.
pub async fn overtime_item(
    pool: &PgPool,
    player_id: Uuid,
    today: NaiveDate,
    item_id: Uuid,
) -> Result<Option<StoredItem>, Error> {
    let row = sqlx::query(concat!(
        "SELECT i.id, i.item_key, i.payload FROM items i WHERE i.id = $3 AND ",
        overtime!()
    ))
    .bind(player_id)
    .bind(today)
    .bind(item_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(stored_item).transpose()
}

/// An item by id, whatever day it was shown.
pub async fn item(pool: &PgPool, item_id: Uuid) -> Result<Option<StoredItem>, Error> {
    let row = sqlx::query("SELECT id, item_key, payload FROM items WHERE id = $1")
        .bind(item_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(stored_item).transpose()
}

/// How many Overtime answers the player has given.
pub async fn overtime_answered(pool: &PgPool, player_id: Uuid) -> Result<i64, Error> {
    sqlx::query_scalar("SELECT count(*) FROM votes WHERE player_id = $1 AND day IS NULL")
        .bind(player_id)
        .fetch_one(pool)
        .await
}

// -- ranks and the leaderboard ------------------------------------------------

/// Days completed, and answers on settled items (and how many matched).
pub async fn player_record(pool: &PgPool, player_id: Uuid) -> Result<(i64, i64, i64), Error> {
    let row = sqlx::query(
        "SELECT
           (SELECT count(*) FROM player_days WHERE player_id = $1 AND completed) AS days,
           count(c.item_id) AS scored,
           count(c.item_id) FILTER (WHERE v.answer = c.label) AS right_answers
         FROM votes v
         JOIN consensus c ON c.item_id = v.item_id AND c.status = 'settled' AND c.label IS NOT NULL
         WHERE v.player_id = $1",
    )
    .bind(player_id)
    .fetch_one(pool)
    .await?;
    Ok((
        row.try_get("days")?,
        row.try_get("scored")?,
        row.try_get("right_answers")?,
    ))
}

/// One row per player with a handle: their daily answers from `since` to
/// `until` on items since settled, how many matched, and whether the scorer
/// has flagged them.
pub async fn weekly_standings(
    pool: &PgPool,
    since: NaiveDate,
    until: NaiveDate,
) -> Result<Vec<(String, i64, i64, bool)>, Error> {
    let rows = sqlx::query(
        "SELECT p.handle, p.flags <> '{}'::jsonb AS flagged,
                count(c.item_id) AS scored,
                count(c.item_id) FILTER (WHERE v.answer = c.label) AS right_answers
         FROM players p
         JOIN votes v ON v.player_id = p.id AND v.day BETWEEN $1 AND $2
         JOIN consensus c ON c.item_id = v.item_id AND c.status = 'settled' AND c.label IS NOT NULL
         WHERE p.handle IS NOT NULL
         GROUP BY p.id",
    )
    .bind(since)
    .bind(until)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("handle")?,
                row.try_get("scored")?,
                row.try_get("right_answers")?,
                row.try_get("flagged")?,
            ))
        })
        .collect()
}

/// The cached leaderboard page for the week starting `week_of`, if it was
/// rendered at or after `fresh_after`.
pub async fn cached_leaderboard(
    pool: &PgPool,
    week_of: NaiveDate,
    fresh_after: DateTime<Utc>,
) -> Result<Option<String>, Error> {
    sqlx::query_scalar(
        "SELECT body FROM leaderboard_cache WHERE week_of = $1 AND computed_at >= $2",
    )
    .bind(week_of)
    .bind(fresh_after)
    .fetch_optional(pool)
    .await
}

/// Stores a freshly rendered leaderboard page, and drops other weeks' pages.
pub async fn cache_leaderboard(
    pool: &PgPool,
    week_of: NaiveDate,
    computed_at: DateTime<Utc>,
    body: &str,
) -> Result<(), Error> {
    sqlx::query("DELETE FROM leaderboard_cache WHERE week_of <> $1")
        .bind(week_of)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO leaderboard_cache (week_of, computed_at, body) VALUES ($1, $2, $3)
         ON CONFLICT (week_of) DO UPDATE SET computed_at = excluded.computed_at, body = excluded.body",
    )
    .bind(week_of)
    .bind(computed_at)
    .bind(body)
    .execute(pool)
    .await?;
    Ok(())
}

/// (handle, days played, settled answers, matching answers) for every player
/// with a handle: what the board needs to put each one in a rank tier.
pub async fn handle_records(pool: &PgPool) -> Result<Vec<(String, i64, i64, i64)>, Error> {
    let rows = sqlx::query(
        "SELECT p.handle,
                (SELECT count(*) FROM player_days d WHERE d.player_id = p.id AND d.completed) AS days,
                count(c.item_id) AS scored,
                count(c.item_id) FILTER (WHERE v.answer = c.label) AS right_answers
         FROM players p
         LEFT JOIN votes v ON v.player_id = p.id
         LEFT JOIN consensus c ON c.item_id = v.item_id AND c.status = 'settled' AND c.label IS NOT NULL
         WHERE p.handle IS NOT NULL
         GROUP BY p.id",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("handle")?,
                row.try_get("days")?,
                row.try_get("scored")?,
                row.try_get("right_answers")?,
            ))
        })
        .collect()
}

/// Takes a handle away from whoever has it, in any case, for a reported or
/// offensive name. Returns false if nobody had it. The player keeps
/// playing; they just drop off the leaderboard until they pick another.
pub async fn clear_handle(pool: &PgPool, handle: &str) -> Result<bool, Error> {
    let result = sqlx::query("UPDATE players SET handle = NULL WHERE lower(handle) = lower($1)")
        .bind(handle)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn handle(pool: &PgPool, player_id: Uuid) -> Result<Option<String>, Error> {
    sqlx::query_scalar("SELECT handle FROM players WHERE id = $1")
        .bind(player_id)
        .fetch_optional(pool)
        .await
        .map(Option::flatten)
}

/// Sets a player's handle. Returns false if another player has it already,
/// in any case.
pub async fn set_handle(pool: &PgPool, player_id: Uuid, handle: &str) -> Result<bool, Error> {
    let result = sqlx::query("UPDATE players SET handle = $2 WHERE id = $1")
        .bind(player_id)
        .bind(handle)
        .execute(pool)
        .await;
    match result {
        Ok(_) => Ok(true),
        Err(Error::Database(err)) if err.is_unique_violation() => Ok(false),
        Err(err) => Err(err),
    }
}
