//! Every query in dorman-db, against a fresh Postgres database per test.

use dorman_core::BatchItem;
use dorman_testkit::{count, day, example_batch, load_example, publish, seed_day, valid_answer};
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
