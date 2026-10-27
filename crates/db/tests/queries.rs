//! Every query in dorman-db, against a fresh Postgres database per test.

use std::borrow::Cow;

use chrono::{DateTime, Duration, Utc};
use dorman_core::{pick_daily_set, Batch, BatchItem, DailyPlan};
use dorman_testkit::{
    approve, count, day, example_batch, load_example, load_pending, publish, seed_day, valid_answer,
};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;
use sqlx::PgPool;

#[sqlx::test(migrations = "./migrations")]
async fn loading_a_batch_twice_adds_nothing_the_second_time(pool: PgPool) {
    let batch = example_batch();
    load_example(&pool).await;
    for item in &batch.items {
        let added = dorman_db::insert_item(&pool, &batch.batch, &batch.intents, item)
            .await
            .unwrap();
        assert!(!added);
    }
    assert_eq!(count(&pool, "items").await, batch.items.len() as i64);
}

#[sqlx::test(migrations = "./migrations")]
async fn intent_items_store_four_options_including_generator_and_gold(pool: PgPool) {
    let items = seed_day(&pool, day("2026-10-27")).await;
    for stored in &items {
        match &stored.item {
            BatchItem::Intent(item) => {
                assert_eq!(stored.options.len(), 4);
                assert!(stored.options.contains(&item.generator_intent));
                if let Some(gold) = &item.gold_answer {
                    assert!(stored.options.contains(gold));
                }
            }
            BatchItem::Pair(_) => assert!(stored.options.is_empty()),
        }
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn gold_answers_are_stored_in_their_own_column(pool: PgPool) {
    load_example(&pool).await;
    let gold: i64 = sqlx::query_scalar("SELECT count(*) FROM items WHERE gold_answer IS NOT NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    let expected = example_batch()
        .items
        .iter()
        .filter(|item| item.gold_answer().is_some())
        .count();
    assert_eq!(gold, expected as i64);
}

#[sqlx::test(migrations = "./migrations")]
async fn an_unpublished_day_has_no_set(pool: PgPool) {
    load_example(&pool).await;
    assert!(dorman_db::daily_set(&pool, day("2026-10-27"))
        .await
        .unwrap()
        .is_none());
}

#[sqlx::test(migrations = "./migrations")]
async fn a_daily_set_keeps_its_published_order_and_can_be_replaced(pool: PgPool) {
    let batch = load_example(&pool).await;
    let mut keys: Vec<String> = batch
        .items
        .iter()
        .map(|i| i.knock_key().to_string())
        .collect();
    keys.reverse();
    let today = day("2026-10-27");

    let items = publish(&pool, today, &keys).await;
    let got: Vec<&str> = items.iter().map(|i| i.knock_key.as_str()).collect();
    assert_eq!(got, keys);

    let items = publish(&pool, today, &keys[..2]).await;
    assert_eq!(items.len(), 2);
    assert_eq!(count(&pool, "daily_sets").await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn pools_split_new_gold_and_unsettled_carry_overs(pool: PgPool) {
    let batch = load_example(&pool).await;
    let (gold, plain): (Vec<_>, Vec<_>) = batch
        .items
        .iter()
        .partition(|item| item.gold_answer().is_some());
    let yesterday = day("2026-10-26");
    let shown: Vec<String> = plain[..2]
        .iter()
        .map(|i| i.knock_key().to_string())
        .chain(gold.iter().map(|i| i.knock_key().to_string()))
        .collect();
    let items = publish(&pool, yesterday, &shown).await;

    // One of yesterday's plain items settled; the other didn't.
    let settled = items
        .iter()
        .find(|i| i.knock_key == plain[0].knock_key())
        .unwrap();
    sqlx::query("INSERT INTO consensus (item_id, label, share, total_weight, status) VALUES ($1, 'x', 0.9, 6, 'settled')")
        .bind(settled.id)
        .execute(&pool)
        .await
        .unwrap();

    let pools = dorman_db::daily_pools(&pool, yesterday).await.unwrap();

    let mut expected_new: Vec<String> = plain[2..]
        .iter()
        .map(|i| i.knock_key().to_string())
        .collect();
    expected_new.sort();
    assert_eq!(pools.new, expected_new);
    assert_eq!(pools.gold.len(), gold.len());
    assert_eq!(pools.carry_over, vec![plain[1].knock_key().to_string()]);
}

#[sqlx::test(migrations = "./migrations")]
async fn carry_overs_come_least_settled_first(pool: PgPool) {
    let batch = load_example(&pool).await;
    let plain: Vec<String> = batch
        .items
        .iter()
        .filter(|i| i.gold_answer().is_none())
        .map(|i| i.knock_key().to_string())
        .collect();
    let yesterday = day("2026-10-26");
    let items = publish(&pool, yesterday, &plain[..2]).await;
    for (item, share) in items.iter().zip([0.7_f32, 0.4]) {
        sqlx::query("INSERT INTO consensus (item_id, share, status) VALUES ($1, $2, 'disputed')")
            .bind(item.id)
            .bind(share)
            .execute(&pool)
            .await
            .unwrap();
    }
    let pools = dorman_db::daily_pools(&pool, yesterday).await.unwrap();
    assert_eq!(pools.carry_over, vec![plain[1].clone(), plain[0].clone()]);
}

#[sqlx::test(migrations = "./migrations")]
async fn players_exist_once_created(pool: PgPool) {
    let id = dorman_db::create_player(&pool).await.unwrap();
    assert!(dorman_db::player_exists(&pool, id).await.unwrap());
    assert!(!dorman_db::player_exists(&pool, uuid::Uuid::new_v4())
        .await
        .unwrap());
    let trust: f32 = sqlx::query_scalar("SELECT trust FROM players WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!((trust - 0.1).abs() < 1e-6, "new players start at 0.1 trust");
}

#[sqlx::test(migrations = "./migrations")]
async fn one_vote_per_player_per_item(pool: PgPool) {
    let today = day("2026-10-27");
    let items = seed_day(&pool, today).await;
    let item = &items[0];
    let player = dorman_db::create_player(&pool).await.unwrap();
    let answer = valid_answer(item);

    assert!(
        dorman_db::record_vote(&pool, player, item.id, Some(today), &answer, 2000)
            .await
            .unwrap()
    );
    assert!(
        !dorman_db::record_vote(&pool, player, item.id, Some(today), "other", 2000)
            .await
            .unwrap()
    );
    assert_eq!(
        dorman_db::player_answers(&pool, player, &[item.id])
            .await
            .unwrap(),
        vec![(item.id, answer)]
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn the_split_counts_answers_most_popular_first(pool: PgPool) {
    let today = day("2026-10-27");
    let items = seed_day(&pool, today).await;
    let item = items.iter().find(|i| i.options.len() == 4).unwrap();
    let (a, b) = (&item.options[0], &item.options[1]);
    for answer in [a, b, b] {
        dorman_testkit::player_with_votes(&pool, today, &[(item.id, answer.as_str())]).await;
    }
    let split = dorman_db::vote_split(&pool, item.id).await.unwrap();
    assert_eq!(split, vec![(b.clone(), 2), (a.clone(), 1)]);
}

#[sqlx::test(migrations = "./migrations")]
async fn deleting_a_player_removes_their_votes(pool: PgPool) {
    let today = day("2026-10-27");
    let items = seed_day(&pool, today).await;
    let answer = valid_answer(&items[0]);
    let player =
        dorman_testkit::player_with_votes(&pool, today, &[(items[0].id, answer.as_str())]).await;
    sqlx::query("DELETE FROM players WHERE id = $1")
        .bind(player)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count(&pool, "votes").await, 0);
}

// -- batch review ---------------------------------------------------------------

/// The example batch under another name, with its knock_keys prefixed so it
/// can be loaded alongside the original.
fn renamed(name: &str) -> Batch {
    let mut batch = example_batch();
    batch.batch = name.to_string();
    for item in &mut batch.items {
        match item {
            BatchItem::Intent(i) => i.knock_key = format!("{name}/{}", i.knock_key),
            BatchItem::Pair(p) => p.knock_key = format!("{name}/{}", p.knock_key),
        }
    }
    batch
}

#[sqlx::test(migrations = "./migrations")]
async fn a_loaded_batch_waits_for_review_until_approved(pool: PgPool) {
    let batch = load_pending(&pool, &example_batch()).await;
    let status = |pool: PgPool| async move {
        dorman_db::batch_status(&pool, &example_batch().batch)
            .await
            .unwrap()
    };
    assert_eq!(status(pool.clone()).await.as_deref(), Some("pending"));
    let pools = dorman_db::daily_pools(&pool, day("2026-10-26"))
        .await
        .unwrap();
    assert!(pools.new.is_empty() && pools.gold.is_empty());

    let items = dorman_db::batch_items(&pool, &batch.batch).await.unwrap();
    assert_eq!(items.len(), batch.items.len());
    assert!(items.windows(2).all(|w| w[0].knock_key < w[1].knock_key));

    let now = Utc::now();
    assert!(dorman_db::approve_batch(&pool, &batch.batch, now)
        .await
        .unwrap());
    assert!(!dorman_db::approve_batch(&pool, &batch.batch, now)
        .await
        .unwrap());
    assert!(!dorman_db::approve_batch(&pool, "no-such-batch", now)
        .await
        .unwrap());
    assert_eq!(status(pool.clone()).await.as_deref(), Some("approved"));
    assert_eq!(
        dorman_db::batch_status(&pool, "no-such-batch")
            .await
            .unwrap(),
        None
    );
    let pools = dorman_db::daily_pools(&pool, day("2026-10-26"))
        .await
        .unwrap();
    assert_eq!(pools.new.len() + pools.gold.len(), batch.items.len());
}

#[sqlx::test(migrations = "./migrations")]
async fn a_daily_set_leaves_out_items_from_unapproved_batches(pool: PgPool) {
    let approved = load_example(&pool).await;
    let pending = load_pending(&pool, &renamed("pending")).await;
    let keys: Vec<String> = pending
        .items
        .iter()
        .chain(&approved.items)
        .map(|i| i.knock_key().to_string())
        .collect();
    let items = publish(&pool, day("2026-10-27"), &keys).await;
    assert_eq!(items.len(), approved.items.len());
    assert!(items.iter().all(|i| !i.knock_key.starts_with("pending/")));

    // Nothing approved at all still publishes, as an empty day.
    assert!(publish(&pool, day("2026-10-28"), &keys[..1])
        .await
        .is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn yesterdays_items_carry_over_only_from_approved_batches(pool: PgPool) {
    load_example(&pool).await;
    let pending = load_pending(&pool, &renamed("pending")).await;
    let yesterday = day("2026-10-26");
    let plain = pending
        .items
        .iter()
        .find(|i| i.gold_answer().is_none())
        .unwrap();
    // Published before the gate, say, by writing the set directly.
    sqlx::query(
        "INSERT INTO daily_sets (day, item_ids)
         SELECT $1, array_agg(id) FROM items WHERE knock_key = $2",
    )
    .bind(yesterday)
    .bind(plain.knock_key())
    .execute(&pool)
    .await
    .unwrap();
    let pools = dorman_db::daily_pools(&pool, yesterday).await.unwrap();
    assert!(pools.carry_over.is_empty());
}

#[sqlx::test(migrations = false)]
async fn batches_already_in_play_stay_approved_after_the_gate_arrives(pool: PgPool) {
    let mut first = sqlx::migrate!("./migrations");
    first.migrations = Cow::Owned(first.migrations[..1].to_vec());
    first.run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO items (id, knock_key, mode, payload, batch)
         VALUES (gen_random_uuid(), 'k1', 'intent', '{}', 'old-batch')",
    )
    .execute(&pool)
    .await
    .unwrap();

    dorman_db::migrate(&pool).await.unwrap();
    assert_eq!(
        dorman_db::batch_status(&pool, "old-batch")
            .await
            .unwrap()
            .as_deref(),
        Some("approved")
    );
}

/// Random pools: several batches of random size, each approved or not, some
/// gold, plus a random yesterday. Whatever the mix, today's set only ever
/// holds items from approved batches. Cases are drawn with proptest but run
/// against one database (cleared between cases), so they don't shrink.
#[sqlx::test(migrations = "./migrations")]
async fn an_unapproved_batch_never_reaches_a_daily_set(pool: PgPool) {
    let batch_strategy = (
        any::<bool>(),
        proptest::collection::vec(any::<bool>(), 1..12),
    );
    let case_strategy = (
        proptest::collection::vec(batch_strategy, 1..5),
        proptest::collection::vec(any::<prop::sample::Index>(), 0..6),
        "[0-9]{4}-[0-9]{2}",
    );
    let mut runner = TestRunner::deterministic();
    let template = example_batch();
    let (intent, pair) = (
        template
            .items
            .iter()
            .find(|i| matches!(i, BatchItem::Intent(_))),
        template
            .items
            .iter()
            .find(|i| matches!(i, BatchItem::Pair(_))),
    );
    for _ in 0..48 {
        let (batches, yesterday_picks, seed) =
            case_strategy.new_tree(&mut runner).unwrap().current();
        sqlx::query("TRUNCATE items, batches, daily_sets CASCADE")
            .execute(&pool)
            .await
            .unwrap();
        let mut approved_keys = Vec::new();
        let mut all_keys = Vec::new();
        for (b, (is_approved, golds)) in batches.iter().enumerate() {
            let mut batch = template.clone();
            batch.batch = format!("b{b}");
            batch.items = golds
                .iter()
                .enumerate()
                .map(|(n, gold)| {
                    let key = format!("b{b}/{n}");
                    let mut item = if n % 3 == 2 { pair } else { intent }.unwrap().clone();
                    match &mut item {
                        BatchItem::Intent(i) => {
                            i.knock_key = key;
                            i.gold_answer = gold.then(|| i.generator_intent.clone());
                        }
                        BatchItem::Pair(p) => {
                            p.knock_key = key;
                            p.gold_answer = gold.then(|| p.replies[0].knock_key.clone());
                        }
                    }
                    item
                })
                .collect();
            load_pending(&pool, &batch).await;
            let keys = batch.items.iter().map(|i| i.knock_key().to_string());
            if *is_approved {
                approve(&pool, &batch.batch).await;
                approved_keys.extend(keys.clone());
            }
            all_keys.extend(keys);
        }
        // Yesterday's set, written directly so it can hold anything.
        let yesterday = day("2026-10-26");
        let shown: Vec<String> = yesterday_picks
            .iter()
            .map(|i| i.get(&all_keys).clone())
            .collect();
        sqlx::query(
            "INSERT INTO daily_sets (day, item_ids)
             SELECT $1, coalesce(array_agg(id), '{}') FROM items WHERE knock_key = ANY ($2)",
        )
        .bind(yesterday)
        .bind(&shown)
        .execute(&pool)
        .await
        .unwrap();

        let pools = dorman_db::daily_pools(&pool, yesterday).await.unwrap();
        let picked = pick_daily_set(
            &seed,
            DailyPlan::default(),
            &pools.new,
            &pools.gold,
            &pools.carry_over,
        );
        let today = publish(&pool, day("2026-10-27"), &picked).await;
        assert_eq!(today.len(), picked.len());
        for item in &today {
            assert!(
                approved_keys.contains(&item.knock_key),
                "{} is from an unapproved batch (case: {batches:?})",
                item.knock_key
            );
        }
        // And publishing every key there is still only lets approved ones in.
        let everything = publish(&pool, day("2026-10-28"), &all_keys).await;
        assert_eq!(everything.len(), approved_keys.len());
    }
}

// -- abuse limits ---------------------------------------------------------------

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn rate_limit_hits_count_per_key_and_window(pool: PgPool) {
    let noon = at("2026-10-27T12:00:00Z");
    let next = noon + Duration::minutes(1);
    for expected in 1..=3 {
        let hits = dorman_db::hit_rate_limit(&pool, "vote", b"a", noon)
            .await
            .unwrap();
        assert_eq!(hits, expected);
    }
    assert_eq!(
        dorman_db::hit_rate_limit(&pool, "vote", b"b", noon)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        dorman_db::hit_rate_limit(&pool, "new_player", b"a", noon)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        dorman_db::hit_rate_limit(&pool, "vote", b"a", next)
            .await
            .unwrap(),
        1
    );

    assert_eq!(dorman_db::purge_rate_limits(&pool, next).await.unwrap(), 3);
    assert_eq!(count(&pool, "rate_limits").await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_token_can_be_claimed_once_until_it_is_forgotten(pool: PgPool) {
    let noon = at("2026-10-27T12:00:00Z");
    let long_ago = noon - Duration::hours(1);
    assert!(dorman_db::claim_token(&pool, b"hash", noon, long_ago)
        .await
        .unwrap());
    assert!(!dorman_db::claim_token(&pool, b"hash", noon, long_ago)
        .await
        .unwrap());
    assert!(dorman_db::claim_token(&pool, b"other", noon, long_ago)
        .await
        .unwrap());

    // Claiming anything later sweeps out hashes from before the cut-off.
    let later = noon + Duration::minutes(20);
    dorman_db::claim_token(&pool, b"third", later, later - Duration::minutes(10))
        .await
        .unwrap();
    assert_eq!(count(&pool, "turnstile_tokens").await, 1);
}
