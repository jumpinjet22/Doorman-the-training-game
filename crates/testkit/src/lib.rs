//! Shared helpers for Dorman's integration tests: seed a database from the
//! example batch, publish days, make players with a vote history, and stub
//! Cloudflare Turnstile.
//!
//! Tests that need Postgres use `#[sqlx::test]`, which creates a fresh
//! database per test from `DATABASE_URL` (a role allowed to create databases).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use dorman_core::turnstile::BoxFuture;
use dorman_core::{Batch, BatchItem, Clock, TurnstileVerifier, Verdict, BOTH_BAD};
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

/// Loads every item in the example batch, as a batch that's been reviewed.
pub async fn load_example(pool: &PgPool) -> Batch {
    let batch = load_pending(pool, &example_batch()).await;
    approve(pool, &batch.batch).await;
    batch
}

/// Loads every item in `batch`, which then waits for review.
pub async fn load_pending(pool: &PgPool, batch: &Batch) -> Batch {
    for item in &batch.items {
        dorman_db::insert_item(pool, &batch.batch, &batch.intents, item)
            .await
            .expect("insert item");
    }
    batch.clone()
}

pub async fn approve(pool: &PgPool, batch: &str) {
    dorman_db::approve_batch(pool, batch, Utc::now())
        .await
        .expect("approve batch");
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
    // Test-only: `table` is always a literal table name written in a test.
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .expect("count rows")
}

/// How long Cloudflare honours a Turnstile token.
pub const TOKEN_LIFETIME: Duration = Duration::minutes(5);

/// A stand-in for Cloudflare: passes tokens it issued that are under five
/// minutes old by `clock`, rejects anything else, and can be taken down.
/// It doesn't track reuse; refusing a replayed token is the app's job.
pub struct StubTurnstile {
    clock: Arc<dyn Clock>,
    issued: Mutex<HashMap<String, DateTime<Utc>>>,
    down: AtomicBool,
    calls: AtomicUsize,
}

impl StubTurnstile {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            issued: Mutex::new(HashMap::new()),
            down: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        }
    }

    /// A fresh token, as the widget in a browser would get.
    pub fn issue(&self) -> String {
        let mut issued = self.issued.lock().expect("stub lock");
        let token = format!("stub-token-{}", issued.len());
        issued.insert(token.clone(), self.clock.now());
        token
    }

    /// Simulates Cloudflare being unreachable.
    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    /// How many times anything asked it to verify a token.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TurnstileVerifier for StubTurnstile {
    fn verify<'a>(&'a self, token: &'a str, _remote_ip: Option<IpAddr>) -> BoxFuture<'a, Verdict> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let verdict = if self.down.load(Ordering::SeqCst) {
            Verdict::Unavailable("stub is down".into())
        } else {
            match self.issued.lock().expect("stub lock").get(token) {
                None => Verdict::Rejected("invalid-input-response".into()),
                Some(at) if self.clock.now() - *at > TOKEN_LIFETIME => {
                    Verdict::Rejected("timeout-or-duplicate".into())
                }
                Some(_) => Verdict::Passed,
            }
        };
        Box::pin(async move { verdict })
    }
}
