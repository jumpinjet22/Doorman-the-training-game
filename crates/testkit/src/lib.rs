//! Shared helpers for Dorman's integration tests: seed a database from the
//! example batch, publish days, and make players with a vote history.
//!
//! Tests that need Postgres use `#[sqlx::test]`, which creates a fresh
//! database per test from `DATABASE_URL` (a role allowed to create databases).

use chrono::NaiveDate;
use dorman_core::{Batch, BatchItem, BOTH_BAD};
use dorman_db::StoredItem;
use sqlx::PgPool;
use uuid::Uuid;

/// A real batch written by KNOCK's `scripts/export_game_batch.py`.
pub const EXAMPLE_BATCH: &str = include_str!("../../../fixtures/batch.example.json");

pub fn example_batch() -> Batch {
    Batch::parse(EXAMPLE_BATCH).expect("the example batch parses")
}

pub fn day(text: &str) -> NaiveDate {
    text.parse().expect("a YYYY-MM-DD date")
}

/// Loads every item in the example batch.
pub async fn load_example(pool: &PgPool) -> Batch {
    let batch = example_batch();
    for item in &batch.items {
        dorman_db::insert_item(pool, &batch.batch, &batch.intents, item)
            .await
            .expect("insert item");
    }
    batch
}

/// Publishes `keys` as the set for `day` and returns the stored items.
pub async fn publish(pool: &PgPool, day: NaiveDate, keys: &[String]) -> Vec<StoredItem> {
    dorman_db::set_daily(pool, day, keys)
        .await
        .expect("publish day");
    dorman_db::daily_set(pool, day)
        .await
        .expect("read day")
        .expect("day exists")
}

/// Loads the example batch and publishes all of it for `day`.
pub async fn seed_day(pool: &PgPool, day: NaiveDate) -> Vec<StoredItem> {
    let batch = load_example(pool).await;
    let keys: Vec<String> = batch
        .items
        .iter()
        .map(|item| item.knock_key().to_string())
        .collect();
    publish(pool, day, &keys).await
}

/// Some answer a player is allowed to give for `item`.
pub fn valid_answer(item: &StoredItem) -> String {
    match &item.item {
        BatchItem::Intent(_) => item.options[0].clone(),
        BatchItem::Pair(_) => BOTH_BAD.to_string(),
    }
}

/// A new player who has already answered `answers` on `day`.
pub async fn player_with_votes(pool: &PgPool, day: NaiveDate, answers: &[(Uuid, &str)]) -> Uuid {
    let player = dorman_db::create_player(pool).await.expect("create player");
    for (item_id, answer) in answers {
        dorman_db::record_vote(pool, player, *item_id, Some(day), answer, 3000)
            .await
            .expect("record vote");
    }
    player
}

pub async fn count(pool: &PgPool, table: &str) -> i64 {
    let sql = format!("SELECT count(*) FROM {table}");
    sqlx::query_scalar(&sql)
        .fetch_one(pool)
        .await
        .expect("count rows")
}
