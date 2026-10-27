//! Postgres access. Queries use sqlx's runtime-checked API rather than the
//! `query!` macros so the workspace builds without a live database; switch to
//! the macros (and `cargo sqlx prepare`) once CI has a Postgres service.

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
    pub knock_key: String,
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
        knock_key: row.try_get("knock_key")?,
        item,
        options,
    })
}

/// Stores one batch item. Returns false when its knock_key is already loaded.
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
            &intent.knock_key,
            &intent.generator_intent,
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
        "INSERT INTO items (id, knock_key, mode, payload, gold_answer, batch)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (knock_key) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(item.knock_key())
    .bind(item.mode())
    .bind(payload)
    .bind(item.gold_answer())
    .bind(batch)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Candidate knock_keys for a new daily set.
#[derive(Debug, Default)]
pub struct DailyPools {
    /// Never in a daily set, no gold answer.
    pub new: Vec<String>,
    pub gold: Vec<String>,
    /// In `before`'s set and still open or disputed, least settled first.
    pub carry_over: Vec<String>,
}

fn knock_keys(rows: &[sqlx::postgres::PgRow]) -> Result<Vec<String>, Error> {
    rows.iter().map(|row| row.try_get("knock_key")).collect()
}

/// SQL true when item `i` comes from a batch a person has approved.
const APPROVED: &str =
    "EXISTS (SELECT 1 FROM batches b WHERE b.name = i.batch AND b.status = 'approved')";

/// Only items from approved batches are candidates.
pub async fn daily_pools(pool: &PgPool, before: NaiveDate) -> Result<DailyPools, Error> {
    let new = sqlx::query(&format!(
        "SELECT knock_key FROM items i
         WHERE gold_answer IS NULL AND {APPROVED}
           AND NOT EXISTS (SELECT 1 FROM daily_sets d WHERE i.id = ANY (d.item_ids))
         ORDER BY knock_key"
    ))
    .fetch_all(pool)
    .await?;
    let gold = sqlx::query(&format!(
        "SELECT knock_key FROM items i
         WHERE gold_answer IS NOT NULL AND {APPROVED}
         ORDER BY knock_key"
    ))
    .fetch_all(pool)
    .await?;
    let carry_over = sqlx::query(&format!(
        "SELECT i.knock_key FROM daily_sets d
         JOIN items i ON i.id = ANY (d.item_ids)
         LEFT JOIN consensus c ON c.item_id = i.id
         WHERE d.day = $1 AND i.gold_answer IS NULL AND {APPROVED}
           AND coalesce(c.status, 'open') <> 'settled'
         ORDER BY coalesce(c.share, 0), i.knock_key"
    ))
    .bind(before)
    .fetch_all(pool)
    .await?;
    Ok(DailyPools {
        new: knock_keys(&new)?,
        gold: knock_keys(&gold)?,
        carry_over: knock_keys(&carry_over)?,
    })
}

/// Publishes the set for `day`, replacing any earlier one for that day.
/// Items from batches that aren't approved are left out.
pub async fn set_daily(pool: &PgPool, day: NaiveDate, knock_keys: &[String]) -> Result<(), Error> {
    sqlx::query(&format!(
        "INSERT INTO daily_sets (day, item_ids)
         SELECT $1, coalesce(array_agg(i.id ORDER BY k.ord), '{{}}')
         FROM unnest($2::text[]) WITH ORDINALITY AS k(key, ord)
         JOIN items i ON i.knock_key = k.key AND {APPROVED}
         ON CONFLICT (day) DO UPDATE SET item_ids = excluded.item_ids, published_at = now()"
    ))
    .bind(day)
    .bind(knock_keys)
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
        "SELECT i.id, i.knock_key, i.payload
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

/// Every item in a batch, in knock_key order, for a person to skim.
pub async fn batch_items(pool: &PgPool, batch: &str) -> Result<Vec<StoredItem>, Error> {
    let rows =
        sqlx::query("SELECT id, knock_key, payload FROM items WHERE batch = $1 ORDER BY knock_key")
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
