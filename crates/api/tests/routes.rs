//! Every route, through the real router and a fresh Postgres database, with
//! a frozen clock.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, Response, StatusCode};
use axum::Router;
use chrono::Duration;
use dorman_api::{app, cookie_key, AppState};
use dorman_core::{BatchItem, FixedClock};
use dorman_db::StoredItem;
use dorman_testkit::{count, day, seed_day, valid_answer};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

const TODAY: &str = "2026-10-27";
const SECRET: &str = "test-secret-test-secret-test-secret";

struct Harness {
    app: Router,
    pool: PgPool,
    clock: Arc<FixedClock>,
}

impl Harness {
    fn new(pool: PgPool) -> Self {
        let clock = Arc::new(FixedClock::at_day(day(TODAY)));
        let state = AppState {
            pool: pool.clone(),
            key: cookie_key(SECRET),
            secure_cookies: true,
            clock: clock.clone(),
        };
        Harness {
            app: app(state),
            pool,
            clock,
        }
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Option<String>, String) {
        let response: Response<Body> = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, cookie, String::from_utf8(body.to_vec()).unwrap())
    }

    async fn get(&self, path: &str, cookie: Option<&str>) -> (StatusCode, Option<String>, String) {
        let mut request = Request::get(path);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        self.send(request.body(Body::empty()).unwrap()).await
    }

    async fn vote(
        &self,
        cookie: Option<&str>,
        body: Value,
    ) -> (StatusCode, Option<String>, String) {
        let mut request =
            Request::post("/api/vote").header(header::CONTENT_TYPE, "application/json");
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        self.send(request.body(Body::from(body.to_string())).unwrap())
            .await
    }
}

fn vote_body(item: &StoredItem, answer: &str) -> Value {
    json!({"item_id": item.id, "answer": answer, "ms_to_answer": 2500})
}

#[sqlx::test(migrations = "../db/migrations")]
async fn healthz_and_static_script(pool: PgPool) {
    let h = Harness::new(pool);
    assert_eq!(h.get("/healthz", None).await.2, "ok");

    let response = h
        .app
        .clone()
        .oneshot(Request::get("/static/play.js").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/javascript; charset=utf-8"
    );
}

#[sqlx::test(migrations = "../db/migrations")]
async fn no_set_today_says_so(pool: PgPool) {
    let h = Harness::new(pool);
    let (status, cookie, body) = h.get("/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("No game today"));
    assert!(cookie.unwrap().starts_with("player="));
}

#[sqlx::test(migrations = "../db/migrations")]
async fn the_page_shows_todays_set_and_keeps_the_same_player(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;

    let (_, cookie, body) = h.get("/", None).await;
    assert_eq!(body.matches("class=\"card\"").count(), items.len());
    assert!(body.contains(r#"<script src="/static/play.js" defer></script>"#));
    let cookie = cookie.unwrap();

    let (_, again, _) = h.get("/", Some(&cookie)).await;
    assert!(again.is_none(), "a known player gets no new cookie");
    assert_eq!(count(&h.pool, "players").await, 1);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn the_cookie_is_locked_down(pool: PgPool) {
    let h = Harness::new(pool);
    let response = h
        .app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    for attribute in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
        assert!(
            cookie.contains(attribute),
            "cookie missing {attribute}: {cookie}"
        );
    }
}

#[sqlx::test(migrations = "../db/migrations")]
async fn a_vote_is_recorded_once_and_returns_the_split(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let item = &items[0];
    let answer = valid_answer(item);
    let (_, cookie, _) = h.get("/", None).await;
    let cookie = cookie.unwrap();

    let (status, _, body) = h.vote(Some(&cookie), vote_body(item, &answer)).await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["recorded"], true);
    assert_eq!(body["split"][0]["answer"], answer.as_str());
    assert_eq!(body["split"][0]["count"], 1);

    let (status, _, body) = h.vote(Some(&cookie), vote_body(item, &answer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["recorded"],
        false
    );
    assert_eq!(count(&h.pool, "votes").await, 1);

    let (_, _, page) = h.get("/", Some(&cookie)).await;
    assert!(
        page.contains(r#"class="mine""#),
        "the answered card shows the choice"
    );
}

#[sqlx::test(migrations = "../db/migrations")]
async fn intent_answers_must_be_one_of_the_shown_options(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let item = items
        .iter()
        .find(|i| matches!(i.item, BatchItem::Intent(_)))
        .unwrap();
    // A real intent, just not one of this item's four options.
    let batch = dorman_testkit::example_batch();
    let hidden = batch
        .intents
        .iter()
        .find(|intent| !item.options.contains(intent))
        .unwrap();

    for answer in [hidden.as_str(), "pizza_party", ""] {
        let (status, _, _) = h.vote(None, vote_body(item, answer)).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "accepted {answer:?}"
        );
    }
    assert_eq!(count(&h.pool, "votes").await, 0);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn pair_answers_are_a_reply_or_both_bad(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let item = items
        .iter()
        .find(|i| matches!(i.item, BatchItem::Pair(_)))
        .unwrap();
    let BatchItem::Pair(pair) = &item.item else {
        unreachable!()
    };

    let (status, _, _) = h.vote(None, vote_body(item, "delivery")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _, _) = h
        .vote(None, vote_body(item, &pair.replies[1].knock_key))
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn items_outside_todays_set_are_refused(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let item = &items[0];
    let answer = valid_answer(item);

    let (status, _, _) = h
        .vote(
            None,
            json!({"item_id": uuid::Uuid::new_v4(), "answer": answer, "ms_to_answer": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Tomorrow, yesterday's items are closed.
    h.clock.advance(Duration::days(1));
    let (status, _, _) = h.vote(None, vote_body(item, &answer)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(count(&h.pool, "votes").await, 0);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn malformed_votes_are_rejected(pool: PgPool) {
    let h = Harness::new(pool);
    seed_day(&h.pool, day(TODAY)).await;
    for body in [
        json!({}),
        json!({"item_id": "nope", "answer": "x", "ms_to_answer": 1}),
    ] {
        let (status, _, _) = h.vote(None, body).await;
        assert!(status.is_client_error(), "got {status}");
    }
    let request = Request::post("/api/vote")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("hello"))
        .unwrap();
    assert!(h.send(request).await.0.is_client_error());
}

#[sqlx::test(migrations = "../db/migrations")]
async fn answer_times_are_clamped(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let (a, b) = (&items[0], &items[1]);
    for (item, ms) in [(a, -50), (b, i32::MAX)] {
        let body = json!({"item_id": item.id, "answer": valid_answer(item), "ms_to_answer": ms});
        assert_eq!(h.vote(None, body).await.0, StatusCode::OK);
    }
    let times: Vec<i32> =
        sqlx::query_scalar("SELECT ms_to_answer FROM votes ORDER BY ms_to_answer")
            .fetch_all(&h.pool)
            .await
            .unwrap();
    assert_eq!(times, vec![0, 600_000]);
}

// -- security --------------------------------------------------------------

#[sqlx::test(migrations = "../db/migrations")]
async fn a_forged_cookie_never_becomes_someone_else(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let victim = dorman_db::create_player(&h.pool).await.unwrap();

    // An unsigned cookie naming the victim, and a signed one with a tampered value.
    let (_, real, _) = h.get("/", None).await;
    let real = real.unwrap();
    let tampered = format!("{}x", &real[..real.len() - 1]);
    for cookie in [format!("player={victim}"), tampered] {
        let (_, fresh, _) = h
            .vote(
                Some(&cookie),
                vote_body(&items[0], &valid_answer(&items[0])),
            )
            .await;
        assert!(fresh.is_some(), "a bad cookie gets a brand-new player");
    }
    let victim_votes: i64 = sqlx::query_scalar("SELECT count(*) FROM votes WHERE player_id = $1")
        .bind(victim)
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(victim_votes, 0);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn no_response_leaks_answers_or_judge_data(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let (_, cookie, page) = h.get("/", None).await;
    let mut bodies = vec![page];
    for item in &items {
        bodies.push(
            h.vote(cookie.as_deref(), vote_body(item, &valid_answer(item)))
                .await
                .2,
        );
    }
    for body in &bodies {
        for field in [
            "gold",
            "generator",
            "judge",
            "category_correct",
            "knock_key",
            "batch",
        ] {
            assert!(!body.contains(field), "response mentions {field}: {body}");
        }
    }
    // A gold answer only ever appears as one of the shown choices, never marked.
    assert!(!bodies[0].contains("example-1"));
}

#[sqlx::test(migrations = "../db/migrations")]
async fn visitor_text_is_escaped(pool: PgPool) {
    let h = Harness::new(pool);
    let mut batch = dorman_testkit::example_batch();
    let BatchItem::Intent(item) = &mut batch.items[0] else {
        panic!("first example item is an intent item")
    };
    item.text = "<script>alert(1)</script>".into();
    item.scene = Some("<img src=x onerror=alert(2)>".into());
    let key = item.knock_key.clone();
    dorman_db::insert_item(&h.pool, &batch.batch, &batch.intents, &batch.items[0])
        .await
        .unwrap();
    dorman_testkit::publish(&h.pool, day(TODAY), &[key]).await;

    let (_, _, page) = h.get("/", None).await;
    assert!(!page.contains("<script>alert"));
    assert!(!page.contains("<img src=x"));
    assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
}
