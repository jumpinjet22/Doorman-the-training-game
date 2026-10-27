//! Every route, through the real router and a fresh Postgres database, with
//! a frozen clock and a stubbed Turnstile.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderName, Request, Response, StatusCode};
use axum::Router;
use chrono::Duration;
use dorman_api::turnstile::CloudflareVerifier;
use dorman_api::{app, cookie_key, network_salt, AppState, Limits, MAX_BODY_BYTES};
use dorman_core::{BatchItem, Clock, FixedClock, TurnstileVerifier};
use dorman_db::StoredItem;
use dorman_testkit::{count, day, seed_day, valid_answer, StubTurnstile};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

const TODAY: &str = "2026-10-27";
/// So today is day 3.
const LAUNCH: &str = "2026-10-25";
const SECRET: &str = "test-secret-test-secret-test-secret";
const ORIGIN: &str = "https://dorman.test";
const SITE_KEY: &str = "1x00000000000000000000AA";

/// Where test requests come from unless a test says otherwise.
fn home_ip() -> IpAddr {
    "203.0.113.7".parse().unwrap()
}

struct Harness {
    app: Router,
    pool: PgPool,
    clock: Arc<FixedClock>,
    turnstile: Arc<StubTurnstile>,
}

#[derive(Default)]
struct Setup {
    limits: Limits,
    client_ip_header: Option<HeaderName>,
    verifier: Option<Arc<dyn TurnstileVerifier>>,
}

impl Harness {
    fn new(pool: PgPool) -> Self {
        Self::with(pool, Setup::default())
    }

    fn with_limits(pool: PgPool, limits: Limits) -> Self {
        Self::with(
            pool,
            Setup {
                limits,
                ..Setup::default()
            },
        )
    }

    fn with(pool: PgPool, setup: Setup) -> Self {
        let clock = Arc::new(FixedClock::at_day(day(TODAY)));
        let turnstile = Arc::new(StubTurnstile::new(clock.clone()));
        let state = AppState {
            pool: pool.clone(),
            key: cookie_key(SECRET),
            secure_cookies: true,
            clock: clock.clone(),
            turnstile: setup.verifier.unwrap_or_else(|| turnstile.clone()),
            turnstile_site_key: SITE_KEY.into(),
            public_origin: ORIGIN.into(),
            client_ip_header: setup.client_ip_header,
            network_salt: network_salt(SECRET),
            limits: setup.limits,
            launch_date: day(LAUNCH),
        };
        Harness {
            app: app(state),
            pool,
            clock,
            turnstile,
        }
    }

    async fn respond(&self, request: Request<Body>) -> Response<Body> {
        self.app.clone().oneshot(request).await.unwrap()
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Option<String>, String) {
        let response = self.respond(request).await;
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

    /// A vote from our own page, with a fresh Turnstile token unless `body`
    /// brings its own.
    async fn vote(
        &self,
        cookie: Option<&str>,
        body: Value,
    ) -> (StatusCode, Option<String>, String) {
        self.vote_from(home_ip(), cookie, body).await
    }

    async fn vote_from(
        &self,
        ip: IpAddr,
        cookie: Option<&str>,
        body: Value,
    ) -> (StatusCode, Option<String>, String) {
        self.send(self.vote_request(ip, cookie, body).body_text())
            .await
    }

    fn vote_request(&self, ip: IpAddr, cookie: Option<&str>, mut body: Value) -> VoteRequest {
        if body.is_object() && body.get("turnstile_token").is_none() {
            body["turnstile_token"] = json!(self.turnstile.issue());
        }
        let mut request = Request::post("/api/vote")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, ORIGIN)
            .extension(ConnectInfo(SocketAddr::new(ip, 50_000)));
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        VoteRequest { request, body }
    }

    /// A brand-new player's cookie, made by voting on `item`.
    async fn new_player(&self, item: &StoredItem) -> String {
        let (status, cookie, _) = self.vote(None, vote_body(item, &valid_answer(item))).await;
        assert_eq!(status, StatusCode::OK);
        cookie.expect("a new player gets a cookie")
    }
}

impl Harness {
    /// A form post to `/vote`, as the browser sends it without `play.js`.
    async fn form_vote(&self, cookie: Option<&str>, fields: &[(&str, &str)]) -> Response<Body> {
        let mut request = Request::post("/vote")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ORIGIN, ORIGIN)
            .extension(ConnectInfo(SocketAddr::new(home_ip(), 50_000)));
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let body = serde_urlencoded::to_string(fields).unwrap();
        self.respond(request.body(Body::from(body)).unwrap()).await
    }

    /// A new player who answers every item in `items` (with the first choice).
    async fn play_all(&self, items: &[StoredItem]) -> String {
        self.play_all_as(None, items).await
    }

    async fn play_all_as(&self, cookie: Option<String>, items: &[StoredItem]) -> String {
        let mut cookie = cookie;
        for item in items {
            let (status, fresh, _) = self
                .vote(cookie.as_deref(), vote_body(item, &valid_answer(item)))
                .await;
            assert_eq!(status, StatusCode::OK);
            cookie = cookie.or(fresh);
        }
        cookie.unwrap()
    }

    /// Logs a scorer run as having happened on `ran`.
    async fn scored(&self, ran: &str) {
        sqlx::query(
            "INSERT INTO scoring_runs (day, started_at, finished_at, players, items,
                 settled, disputed, brigaded, export_path, config)
             VALUES ($1, now(), now(), 0, 0, 0, 0, 0, '', '{}')",
        )
        .bind(day(ran))
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn verdict(&self, item: &StoredItem, status: &str, label: Option<&str>, split: Value) {
        sqlx::query(
            "INSERT INTO consensus (item_id, label, share, total_weight, status, split)
             VALUES ($1, $2, 0.8, 6, $3, $4)",
        )
        .bind(item.id)
        .bind(label)
        .bind(status)
        .bind(split)
        .execute(&self.pool)
        .await
        .unwrap();
    }
}

struct VoteRequest {
    request: axum::http::request::Builder,
    body: Value,
}

impl VoteRequest {
    fn body_text(self) -> Request<Body> {
        self.request
            .body(Body::from(self.body.to_string()))
            .unwrap()
    }
}

fn vote_body(item: &StoredItem, answer: &str) -> Value {
    json!({"item_id": item.id, "answer": answer, "ms_to_answer": 2500})
}

fn with_token(item: &StoredItem, token: &str) -> Value {
    let mut body = vote_body(item, &valid_answer(item));
    body["turnstile_token"] = json!(token);
    body
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
    let response = h
        .respond(
            Request::get("/static/play.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/css; charset=utf-8"
    );
}

#[sqlx::test(migrations = "../db/migrations")]
async fn no_set_today_says_so(pool: PgPool) {
    let h = Harness::new(pool);
    let (status, cookie, body) = h.get("/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("No game today"));
    assert!(cookie.is_none(), "looking doesn't make a player");
    assert_eq!(count(&h.pool, "players").await, 0);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn the_page_shows_todays_set_and_keeps_the_same_player(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;

    let (_, cookie, body) = h.get("/", None).await;
    assert!(cookie.is_none());
    assert_eq!(
        body.matches("class=\"card\"").count(),
        1,
        "one card at a time"
    );
    assert!(body.contains(&format!("Card 1 of {}", items.len())));
    assert!(body.contains(r#"<script src="/static/play.js" defer></script>"#));
    assert!(body.contains(&format!(r#"data-sitekey="{SITE_KEY}""#)));
    assert_eq!(count(&h.pool, "players").await, 0);

    let cookie = h.new_player(&items[0]).await;
    let (_, again, _) = h.get("/", Some(&cookie)).await;
    assert!(again.is_none(), "a known player gets no new cookie");
    let (_, again, _) = h
        .vote(
            Some(&cookie),
            vote_body(&items[1], &valid_answer(&items[1])),
        )
        .await;
    assert!(again.is_none());
    assert_eq!(count(&h.pool, "players").await, 1);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn the_cookie_is_locked_down(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let response = h
        .respond(
            h.vote_request(
                home_ip(),
                None,
                vote_body(&items[0], &valid_answer(&items[0])),
            )
            .body_text(),
        )
        .await;
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

    let (status, cookie, body) = h.vote(None, vote_body(item, &answer)).await;
    assert_eq!(status, StatusCode::OK);
    let cookie = cookie.unwrap();
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["recorded"], true);
    assert_eq!(body["split"][0]["answer"], answer.as_str());
    assert_eq!(body["split"][0]["count"], 1);
    assert_eq!(body["remaining"], items.len() - 1);

    let (status, _, body) = h.vote(Some(&cookie), vote_body(item, &answer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["recorded"],
        false
    );
    assert_eq!(count(&h.pool, "votes").await, 1);

    let (_, _, page) = h.get(&format!("/?after={}", item.id), Some(&cookie)).await;
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
    let real = h.new_player(&items[1]).await;
    let tampered = format!("{}x", &real[..real.len() - 1]);
    for cookie in [format!("player={victim}"), tampered] {
        let (_, fresh, _) = h
            .vote(
                Some(&cookie),
                vote_body(&items[0], &valid_answer(&items[0])),
            )
            .await;
        assert!(fresh.is_some(), "a bad cookie gets a brand-new player");
        let (_, none, _) = h.get("/", Some(&cookie)).await;
        assert!(
            none.is_none(),
            "and looking with one makes no player at all"
        );
    }
    assert_eq!(count(&h.pool, "players").await, 4);
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
    let mut bodies = vec![h.get("/", None).await.2];
    let cookie = h.new_player(&items[0]).await;
    bodies.push(
        h.get(&format!("/?after={}", items[0].id), Some(&cookie))
            .await
            .2,
    );
    for item in &items[1..] {
        bodies.push(
            h.vote(Some(&cookie), vote_body(item, &valid_answer(item)))
                .await
                .2,
        );
    }
    let refused = h.form_vote(Some(&cookie), &[("item_id", "nope")]).await;
    bodies.push(text(refused).await);
    bodies.push(h.get("/results", Some(&cookie)).await.2);
    bodies.push(h.get("/about", None).await.2);
    bodies.push(h.get("/leaderboard", None).await.2);
    bodies.push(h.get("/overtime", Some(&cookie)).await.2);
    // Yesterday, fully scored, with every item decided on.
    for item in &items {
        let answer = valid_answer(item);
        h.verdict(
            item,
            "settled",
            Some(&answer),
            json!({ answer.clone(): 3.0 }),
        )
        .await;
    }
    h.clock.advance(Duration::days(1));
    h.scored("2026-10-28").await;
    bodies.push(h.get("/yesterday", Some(&cookie)).await.2);
    bodies.push(h.get("/", Some(&cookie)).await.2);
    for body in &bodies {
        for field in [
            "gold",
            "generator",
            "judge",
            "category_correct",
            "knock_key",
            "batch",
            "example-1",
        ] {
            assert!(!body.contains(field), "response mentions {field}: {body}");
        }
    }
}

async fn text(response: Response<Body>) -> String {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(body.to_vec()).unwrap()
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
    dorman_testkit::approve(&h.pool, &batch.batch).await;
    dorman_testkit::publish(&h.pool, day(TODAY), &[key]).await;

    let (_, _, page) = h.get("/", None).await;
    assert!(!page.contains("<script>alert"));
    assert!(!page.contains("<img src=x"));
    assert!(page.contains(&escaped("<script>alert(1)</script>")));
}

// -- Turnstile ----------------------------------------------------------------

/// Nothing a refused vote could have written.
async fn assert_nothing_written(h: &Harness) {
    assert_eq!(count(&h.pool, "votes").await, 0);
    assert_eq!(count(&h.pool, "players").await, 0);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn missing_and_malformed_tokens_never_reach_cloudflare(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let mut no_token = vote_body(&items[0], &valid_answer(&items[0]));
    no_token["turnstile_token"] = Value::Null;
    no_token.as_object_mut().unwrap().remove("turnstile_token");
    let request = Request::post("/api/vote")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, ORIGIN)
        .body(Body::from(no_token.to_string()))
        .unwrap();
    assert_eq!(h.send(request).await.0, StatusCode::FORBIDDEN);

    for token in [String::new(), "x".repeat(2049), "tökén".into()] {
        let (status, cookie, _) = h.vote(None, with_token(&items[0], &token)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "accepted {token:?}");
        assert!(cookie.is_none());
    }
    assert_eq!(h.turnstile.calls(), 0);
    assert_nothing_written(&h).await;
}

#[sqlx::test(migrations = "../db/migrations")]
async fn forged_tokens_are_rejected(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    for token in ["stub-token-999", "XXXX.DUMMY.TOKEN.XXXX", "0.forged"] {
        let (status, _, _) = h.vote(None, with_token(&items[0], token)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "accepted {token:?}");
    }
    assert_eq!(h.turnstile.calls(), 3);
    assert_nothing_written(&h).await;
}

#[sqlx::test(migrations = "../db/migrations")]
async fn expired_tokens_are_rejected(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let token = h.turnstile.issue();
    h.clock.advance(Duration::minutes(6));
    let (status, _, _) = h.vote(None, with_token(&items[0], &token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_nothing_written(&h).await;
}

#[sqlx::test(migrations = "../db/migrations")]
async fn a_replayed_token_is_rejected_even_if_cloudflare_would_pass_it(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let token = h.turnstile.issue();
    let (status, cookie, _) = h.vote(None, with_token(&items[0], &token)).await;
    assert_eq!(status, StatusCode::OK);

    // The stub would still pass it; the app remembers it was spent.
    let calls = h.turnstile.calls();
    for cookie in [None, cookie.as_deref()] {
        let (status, _, _) = h.vote(cookie, with_token(&items[1], &token)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        h.turnstile.calls(),
        calls,
        "a replay never reaches Cloudflare"
    );
    assert_eq!(count(&h.pool, "votes").await, 1);
    assert_eq!(count(&h.pool, "players").await, 1);
    // Spent tokens are stored hashed, never as given.
    let stored: Vec<Vec<u8>> = sqlx::query_scalar("SELECT token_hash FROM turnstile_tokens")
        .fetch_all(&h.pool)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].len(), 32);
    assert_ne!(stored[0], token.as_bytes());
}

#[sqlx::test(migrations = "../db/migrations")]
async fn a_cloudflare_outage_blocks_votes(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let cookie = h.new_player(&items[0]).await;
    h.turnstile.set_down(true);
    for cookie in [None, Some(cookie.as_str())] {
        let (status, _, _) = h
            .vote(cookie, vote_body(&items[1], &valid_answer(&items[1])))
            .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
    assert_eq!(count(&h.pool, "votes").await, 1);
    assert_eq!(count(&h.pool, "players").await, 1);

    h.turnstile.set_down(false);
    let (status, _, _) = h
        .vote(
            Some(&cookie),
            vote_body(&items[1], &valid_answer(&items[1])),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn the_real_verifier_fails_closed_when_cloudflare_is_unreachable(pool: PgPool) {
    // Nothing listens on port 9 (discard) here, so the connection is refused.
    let verifier = CloudflareVerifier::with_url("secret".into(), "http://127.0.0.1:9/".into());
    let h = Harness::with(
        pool,
        Setup {
            verifier: Some(Arc::new(verifier)),
            ..Setup::default()
        },
    );
    let items = seed_day(&h.pool, day(TODAY)).await;
    let (status, _, _) = h
        .vote(None, vote_body(&items[0], &valid_answer(&items[0])))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_nothing_written(&h).await;
}

// -- rate limits --------------------------------------------------------------

#[sqlx::test(migrations = "../db/migrations")]
async fn votes_per_minute_are_limited_until_the_window_passes(pool: PgPool) {
    let h = Harness::with_limits(
        pool,
        Limits {
            votes_per_minute: 3,
            ..Limits::default()
        },
    );
    let items = seed_day(&h.pool, day(TODAY)).await;
    let cookie = h.new_player(&items[0]).await;
    for item in &items[1..3] {
        let (status, _, _) = h
            .vote(Some(&cookie), vote_body(item, &valid_answer(item)))
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    // Noon exactly, so a whole minute is left in the window.
    let response = h
        .respond(
            h.vote_request(
                home_ip(),
                Some(&cookie),
                vote_body(&items[3], &valid_answer(&items[3])),
            )
            .body_text(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[header::RETRY_AFTER], "60");
    assert_eq!(count(&h.pool, "votes").await, 3);

    // Another player isn't held up by this one.
    h.new_player(&items[3]).await;

    h.clock.advance(Duration::seconds(59));
    let (status, _, _) = h
        .vote(
            Some(&cookie),
            vote_body(&items[3], &valid_answer(&items[3])),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    h.clock.advance(Duration::seconds(1));
    let (status, _, _) = h
        .vote(
            Some(&cookie),
            vote_body(&items[3], &valid_answer(&items[3])),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count(&h.pool, "votes").await, 5);
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

#[sqlx::test(migrations = "../db/migrations")]
async fn new_players_per_network_are_limited_until_the_hour_passes(pool: PgPool) {
    let h = Harness::with_limits(
        pool,
        Limits {
            new_players_per_hour: 2,
            ..Limits::default()
        },
    );
    let items = seed_day(&h.pool, day(TODAY)).await;
    let vote = |addr: &'static str| {
        let h = &h;
        let item = &items[0];
        async move {
            h.vote_from(ip(addr), None, vote_body(item, &valid_answer(item)))
                .await
        }
    };
    let (status, first, _) = vote("203.0.113.5").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(vote("203.0.113.77").await.0, StatusCode::OK);
    let (status, cookie, _) = vote("203.0.113.200").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(cookie.is_none());
    assert_eq!(count(&h.pool, "players").await, 2);

    // Another /24 is counted on its own, and players who already exist
    // keep playing from the busy one.
    assert_eq!(vote("203.0.114.1").await.0, StatusCode::OK);
    let (status, _, _) = h
        .vote_from(
            ip("203.0.113.9"),
            first.as_deref(),
            vote_body(&items[1], &valid_answer(&items[1])),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    h.clock.advance(Duration::hours(1));
    assert_eq!(vote("203.0.113.200").await.0, StatusCode::OK);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn ipv6_clients_are_counted_by_slash_48(pool: PgPool) {
    let h = Harness::with_limits(
        pool,
        Limits {
            new_players_per_hour: 1,
            ..Limits::default()
        },
    );
    let items = seed_day(&h.pool, day(TODAY)).await;
    let body = vote_body(&items[0], &valid_answer(&items[0]));
    let (h, body) = (&h, &body);
    let status = |addr| async move { h.vote_from(ip(addr), None, body.clone()).await.0 };
    assert_eq!(status("2001:db8:1:1::1").await, StatusCode::OK);
    assert_eq!(
        status("2001:db8:1:ffff::2").await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(status("2001:db8:2::1").await, StatusCode::OK);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn networks_are_stored_only_as_salted_hashes_for_a_day(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    h.vote_from(
        ip("198.51.100.23"),
        None,
        vote_body(&items[0], &valid_answer(&items[0])),
    )
    .await;

    let rows: Vec<(String, Vec<u8>)> =
        sqlx::query_as("SELECT scope, key FROM rate_limits WHERE scope = 'new_player'")
            .fetch_all(&h.pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 1);
    let key = &rows[0].1;
    assert_eq!(key.len(), 32);
    assert!(
        !key.windows(3).any(|w| w == [198, 51, 100]),
        "raw prefix stored"
    );
    let dump: String =
        sqlx::query_scalar("SELECT coalesce(string_agg(t::text, ' '), '') FROM rate_limits t")
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert!(!dump.contains("198.51.100"));

    // A day later, the next new player sweeps the old rows away.
    let tomorrow =
        dorman_testkit::publish(&h.pool, day("2026-10-28"), &[items[0].knock_key.clone()]).await;
    h.clock.advance(Duration::hours(25));
    let (status, _, _) = h
        .vote_from(
            ip("192.0.2.1"),
            None,
            vote_body(&tomorrow[0], &valid_answer(&tomorrow[0])),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let left: Vec<Vec<u8>> = sqlx::query_scalar("SELECT key FROM rate_limits")
        .fetch_all(&h.pool)
        .await
        .unwrap();
    assert!(
        !left.contains(key),
        "yesterday's network hash is still stored"
    );
}

#[sqlx::test(migrations = "../db/migrations")]
async fn behind_a_proxy_the_client_address_comes_from_its_header(pool: PgPool) {
    let h = Harness::with(
        pool,
        Setup {
            limits: Limits {
                new_players_per_hour: 1,
                ..Limits::default()
            },
            client_ip_header: Some(HeaderName::from_static("x-forwarded-for")),
            ..Setup::default()
        },
    );
    let items = seed_day(&h.pool, day(TODAY)).await;
    let from = |forwarded: &'static str| {
        let h = &h;
        let item = &items[0];
        async move {
            let mut request = h.vote_request(home_ip(), None, vote_body(item, &valid_answer(item)));
            request.request = request.request.header("x-forwarded-for", forwarded);
            h.send(request.body_text()).await.0
        }
    };
    assert_eq!(from("10.0.0.1, 198.51.100.1").await, StatusCode::OK);
    // The client can forge the first entries, not the one the proxy added.
    assert_eq!(
        from("10.9.9.9, 198.51.100.2").await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(from("198.51.100.3, 192.0.2.4").await, StatusCode::OK);
    // No usable header: everyone without one shares a bucket.
    assert_eq!(from("not an address").await, StatusCode::OK);
    assert_eq!(from("").await, StatusCode::TOO_MANY_REQUESTS);
}

// -- request hardening --------------------------------------------------------

#[sqlx::test(migrations = "../db/migrations")]
async fn cross_origin_posts_are_refused(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let body = vote_body(&items[0], &valid_answer(&items[0]));
    let attempts: [&[&str]; 6] = [
        &[],
        &["https://evil.test"],
        &["null"],
        &["https://dorman.test.evil.test"],
        &["http://dorman.test"],
        &[ORIGIN, "https://evil.test"],
    ];
    for origins in attempts {
        let mut request = Request::post("/api/vote")
            .header(header::CONTENT_TYPE, "application/json")
            .extension(ConnectInfo(SocketAddr::new(home_ip(), 1)));
        for origin in origins {
            request = request.header(header::ORIGIN, *origin);
        }
        let mut body = body.clone();
        body["turnstile_token"] = json!(h.turnstile.issue());
        let request = request.body(Body::from(body.to_string())).unwrap();
        assert_eq!(
            h.send(request).await.0,
            StatusCode::FORBIDDEN,
            "{origins:?}"
        );
    }
    assert_eq!(h.turnstile.calls(), 0);
    assert_nothing_written(&h).await;
}

#[sqlx::test(migrations = "../db/migrations")]
async fn oversized_bodies_are_refused(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let mut body = vote_body(&items[0], &valid_answer(&items[0]));
    body["padding"] = json!("x".repeat(MAX_BODY_BYTES));
    let (status, _, _) = h.vote(None, body).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(h.turnstile.calls(), 0);
    assert_nothing_written(&h).await;
}

#[sqlx::test(migrations = "../db/migrations")]
async fn every_response_carries_the_security_headers(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let responses = [
        h.respond(Request::get("/").body(Body::empty()).unwrap())
            .await,
        h.respond(Request::get("/static/play.js").body(Body::empty()).unwrap())
            .await,
        h.respond(Request::get("/nope").body(Body::empty()).unwrap())
            .await,
        h.respond(
            h.vote_request(
                home_ip(),
                None,
                vote_body(&items[0], &valid_answer(&items[0])),
            )
            .body_text(),
        )
        .await,
    ];
    for response in &responses {
        let headers = response.headers();
        let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.contains("default-src 'none'"));
        assert!(csp.contains("script-src 'self' https://challenges.cloudflare.com;"));
        assert!(csp.contains("style-src 'self';"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(!csp.contains("unsafe"), "{csp}");
        assert!(headers[header::STRICT_TRANSPORT_SECURITY]
            .to_str()
            .unwrap()
            .starts_with("max-age=63072000"));
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers[header::REFERRER_POLICY], "strict-origin");
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
    }
    // Pages and answers are personal; the script is the same for everyone.
    let cache = |i: usize| {
        responses[i].headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
    };
    assert_eq!(cache(0), "no-store");
    assert_eq!(cache(1), "public, max-age=300");
    assert_eq!(cache(3), "no-store");
}

#[sqlx::test(migrations = "../db/migrations")]
async fn the_page_has_no_inline_script_or_style(pool: PgPool) {
    let h = Harness::new(pool);
    seed_day(&h.pool, day(TODAY)).await;
    let (_, _, page) = h.get("/", None).await;
    let scripts: Vec<&str> = page
        .match_indices("<script")
        .map(|(i, _)| &page[i..])
        .collect();
    assert_eq!(scripts.len(), 2);
    for script in scripts {
        let tag = &script[..script.find('>').unwrap()];
        assert!(
            tag.contains(" src=\"/static/")
                || tag.contains(" src=\"https://challenges.cloudflare.com/")
        );
        assert!(
            script[tag.len()..].starts_with("></script>"),
            "inline script: {tag}"
        );
    }
    for inline in ["<style", " style=", " onclick=", " onload=", "javascript:"] {
        assert!(!page.contains(inline), "page has {inline}");
    }
}

// -- the game -------------------------------------------------------------------

/// The visitor's line as it appears in a page, HTML-escaped.
/// `text` as askama's HTML escaper writes it, so assertions don't depend on
/// which entity spelling the template engine happens to use.
fn escaped(text: &str) -> String {
    askama::filters::escape(text, askama::filters::Html)
        .expect("escaping can't fail")
        .to_string()
}

fn line(item: &StoredItem) -> String {
    let text = match &item.item {
        BatchItem::Intent(i) => &i.text,
        BatchItem::Pair(p) => &p.text,
    };
    escaped(text)
}

/// The card on a page, by its item id.
fn shows_card(page: &str, item: &StoredItem) -> bool {
    page.contains(&format!(r#"data-item="{}""#, item.id))
}

#[sqlx::test(migrations = "../db/migrations")]
async fn one_card_at_a_time_in_order_and_a_reload_resumes(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let total = items.len();

    let (_, _, page) = h.get("/", None).await;
    assert!(shows_card(&page, &items[0]));
    assert!(page.contains(&format!(
        r#"<progress class="progress" value="1" max="{total}""#
    )));
    assert!(page.contains("Day 3"), "launch was two days ago");

    // Answering out of order still resumes at the first one left.
    let cookie = h.new_player(&items[2]).await;
    let (_, _, page) = h.get("/", Some(&cookie)).await;
    assert!(shows_card(&page, &items[0]));
    assert!(page.contains(&format!("Card 1 of {total}")));

    for (n, item) in items.iter().enumerate().filter(|(n, _)| *n != 2) {
        let (_, _, page) = h.get("/", Some(&cookie)).await;
        assert!(shows_card(&page, item), "card {n}");
        assert_eq!(page.matches(r#"class="card""#).count(), 1);
        let last = n == total - 1;
        assert_eq!(page.contains("See your results"), last, "card {n}");
        h.vote(Some(&cookie), vote_body(item, &valid_answer(item)))
            .await;
    }
    let (_, _, page) = h.get("/", Some(&cookie)).await;
    assert!(page.contains("Today's results"));
    assert!(!page.contains(r#"class="card""#));
}

#[sqlx::test(migrations = "../db/migrations")]
async fn a_card_works_as_a_plain_form_post(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let item = &items[0];
    let answer = valid_answer(item);
    let (_, _, page) = h.get("/", None).await;
    let shown_at = h.clock.now().timestamp_millis();
    assert!(page.contains(&format!(r#"name="shown_at" value="{shown_at}""#)));
    assert!(page.contains(r#"<form class="vote" method="post" action="/vote">"#));

    h.clock.advance(Duration::seconds(4));
    let token = h.turnstile.issue();
    let id = item.id.to_string();
    let shown = shown_at.to_string();
    let response = h
        .form_vote(
            None,
            &[
                ("item_id", &id),
                ("answer", &answer),
                ("shown_at", &shown),
                ("cf-turnstile-response", &token),
            ],
        )
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()[header::LOCATION],
        format!("/?after={id}")
    );
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let ms: i32 = sqlx::query_scalar("SELECT ms_to_answer FROM votes")
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(ms, 4000, "timed from when the card was shown");

    // The answered card, with the split and the way on.
    let (_, _, page) = h.get(&format!("/?after={id}"), Some(&cookie)).await;
    assert!(shows_card(&page, item));
    assert!(page.contains(r#"class="mine""#));
    assert!(page.contains("100%"));
    assert!(page.contains(r#"<a class="next" href="/">Next card</a>"#));
    assert!(!page.contains("<form"));
}

#[sqlx::test(migrations = "../db/migrations")]
async fn a_refused_form_post_explains_itself(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let id = items[0].id.to_string();
    let answer = valid_answer(&items[0]);
    let fresh = h.turnstile.issue();
    for (fields, status) in [
        (
            vec![
                ("item_id", id.as_str()),
                ("answer", answer.as_str()),
                ("shown_at", "0"),
            ],
            StatusCode::FORBIDDEN,
        ),
        (
            vec![
                ("item_id", &id),
                ("answer", "pizza_party"),
                ("shown_at", "0"),
                ("cf-turnstile-response", &fresh),
            ],
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let response = h.form_vote(None, &fields).await;
        assert_eq!(response.status(), status);
        let page = text(response).await;
        assert!(page.contains("That didn't go through"));
        assert!(page.contains(r#"<a href="/">Back to the game</a>"#));
    }
    // Without our Origin, it never gets that far.
    let request = Request::post("/vote")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!(
            "item_id={id}&answer={answer}&shown_at=0"
        )))
        .unwrap();
    assert_eq!(h.send(request).await.0, StatusCode::FORBIDDEN);
    assert_nothing_written(&h).await;
}

#[sqlx::test(migrations = "../db/migrations")]
async fn the_split_is_never_shown_for_a_card_not_yet_answered(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    h.play_all(&items[..1]).await; // Someone else's vote is on items[0].
    let cookie = h.new_player(&items[1]).await;
    let (_, _, page) = h
        .get(&format!("/?after={}", items[0].id), Some(&cookie))
        .await;
    assert!(shows_card(&page, &items[0]), "just the card, to answer");
    assert!(page.contains("<form"));
    assert!(!page.contains("%</span>"));
}

#[sqlx::test(migrations = "../db/migrations")]
async fn results_wait_for_the_last_vote(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let see_other =
        |response: (StatusCode, Option<String>, String)| response.0 == StatusCode::SEE_OTHER;
    assert!(see_other(h.get("/results", None).await));
    let cookie = h.play_all(&items[..items.len() - 1]).await;
    assert!(see_other(h.get("/results", Some(&cookie)).await));

    h.play_all_as(Some(cookie.clone()), &items[items.len() - 1..])
        .await;
    let (status, _, page) = h.get("/results", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let squares = "⬜".repeat(items.len());
    assert!(page.contains(&format!("🚪 #3  {squares}")), "{page}");
    assert!(page.contains("Grey squares fill in tomorrow morning"));
    assert_eq!(page.matches(r#"class="result""#).count(), items.len());
    assert_eq!(page.matches("You said:").count(), items.len());
    for item in &items {
        assert!(page.contains(&line(item)));
    }
    // A finished player's front page is the same results.
    assert_eq!(h.get("/", Some(&cookie)).await.2, page);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn a_streak_counts_days_in_a_row_and_a_missed_day_resets_it(pool: PgPool) {
    let h = Harness::new(pool);
    let batch = dorman_testkit::load_example(&h.pool).await;
    let keys: Vec<String> = batch
        .items
        .iter()
        .map(|i| i.knock_key().to_string())
        .collect();
    let day_text = |offset: i64| (day(TODAY) + Duration::days(offset)).to_string();

    let first = dorman_testkit::publish(&h.pool, day(&day_text(0)), &keys[0..2]).await;
    let cookie = h.play_all(&first).await;
    let (_, _, page) = h.get("/results", Some(&cookie)).await;
    assert!(
        page.contains("🚪 #3  ⬜⬜</pre>"),
        "a first day has no streak line"
    );

    h.clock.advance(Duration::days(1));
    let second = dorman_testkit::publish(&h.pool, day(&day_text(1)), &keys[2..4]).await;
    h.play_all_as(Some(cookie.clone()), &second).await;
    let (_, _, page) = h.get("/results", Some(&cookie)).await;
    assert!(page.contains("🚪 #4  ⬜⬜\n🔥 2-day streak"), "{page}");

    // Skip a day, then play.
    h.clock.advance(Duration::days(2));
    let fourth = dorman_testkit::publish(&h.pool, day(&day_text(3)), &keys[4..6]).await;
    h.play_all_as(Some(cookie.clone()), &fourth).await;
    let (_, _, page) = h.get("/results", Some(&cookie)).await;
    assert!(
        page.contains("🚪 #6  ⬜⬜</pre>"),
        "the streak starts over: {page}"
    );
}

#[sqlx::test(migrations = "../db/migrations")]
async fn no_game_counts_down_to_midnight_utc(pool: PgPool) {
    let h = Harness::new(pool);
    let (_, _, page) = h.get("/", None).await;
    assert!(
        page.contains(r#"<time id="countdown" datetime="2026-10-28T00:00:00Z">12 h 0 min</time>"#)
    );
    h.clock
        .advance(Duration::hours(11) + Duration::minutes(59) + Duration::seconds(30));
    let (_, _, page) = h.get("/", None).await;
    assert!(page.contains(">1 min</time>"));
}

#[sqlx::test(migrations = "../db/migrations")]
async fn before_launch_there_is_no_day_number(pool: PgPool) {
    let h = Harness::new(pool);
    seed_day(&h.pool, day(TODAY)).await;
    h.clock
        .set(day("2026-10-24").and_hms_opt(12, 0, 0).unwrap().and_utc());
    seed_day(&h.pool, day("2026-10-24")).await;
    let (_, _, page) = h.get("/", None).await;
    assert!(page.contains(r#"<p class="day"><time datetime="2026-10-24">"#));
    assert!(!page.contains("Day "));
}

#[sqlx::test(migrations = "../db/migrations")]
async fn the_about_page_explains_the_data(pool: PgPool) {
    let h = Harness::new(pool);
    let (status, _, page) = h.get("/about", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("What your answers are for"));
    assert!(page.contains("We never store your IP address"));
}

#[sqlx::test(migrations = "../db/migrations")]
async fn a_day_of_cards_already_answered_still_counts_for_the_streak(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let cookie = h.play_all(&items).await;
    // Tomorrow's set is items they've answered before, so there's nothing
    // to vote on: opening the day is enough.
    h.clock.advance(Duration::days(1));
    let keys: Vec<String> = items.iter().map(|i| i.knock_key.clone()).collect();
    dorman_testkit::publish(&h.pool, day("2026-10-28"), &keys).await;
    let (_, _, page) = h.get("/", Some(&cookie)).await;
    assert!(page.contains("🔥 2-day streak"), "{page}");
    assert_eq!(count(&h.pool, "votes").await, items.len() as i64);
}

// -- gold that can't be told apart --------------------------------------------------

/// What the scorer writes for every item, gold included: settled on `label`
/// with `split`, or open.
async fn decide(h: &Harness, item: &StoredItem, label: Option<&str>, split: Value) {
    let status = if label.is_some() { "settled" } else { "open" };
    h.verdict(item, status, label, split).await;
}

/// The item ids a page lists as results, in order.
fn listed(page: &str) -> Vec<String> {
    page.match_indices(r#"<li class="result" data-item=""#)
        .map(|(i, m)| page[i + m.len()..i + m.len() + 36].to_string())
        .collect()
}

#[sqlx::test(migrations = "../db/migrations")]
async fn the_card_fills_in_the_same_way_for_every_item(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let cookie = h.play_all(&items).await;
    // Every item, gold or not, settled one of three ways against our answer.
    let mut expected = String::new();
    for (n, item) in items.iter().enumerate() {
        let mine = valid_answer(item);
        let (label, split, square) = match n % 3 {
            0 => (mine.clone(), json!({ mine.clone(): 4.0, "x": 1.0 }), '🟩'),
            1 => (
                "x".to_string(),
                json!({ "x": 3.0, mine.clone(): 1.5 }),
                '🟨',
            ),
            _ => (
                "x".to_string(),
                json!({ "x": 9.0, mine.clone(): 1.0 }),
                '🟥',
            ),
        };
        decide(&h, item, Some(&label), split).await;
        expected.push(square);
    }
    assert!(items.iter().any(|i| i.item.gold_answer().is_some()));

    // Scored during the day doesn't count yet.
    h.scored(TODAY).await;
    let (_, _, page) = h.get("/results", Some(&cookie)).await;
    assert!(page.contains(&"⬜".repeat(items.len())));

    h.clock.advance(Duration::days(1));
    h.scored("2026-10-28").await;
    let (_, _, page) = h.get("/yesterday", Some(&cookie)).await;
    assert!(
        page.contains(&format!("🚪 #3  {expected}")),
        "want {expected}: {page}"
    );
}

#[sqlx::test(migrations = "../db/migrations")]
async fn the_card_cant_single_out_gold(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let cookie = h.play_all(&items).await;
    // No rows at all: gold and non-gold alike stay pending.
    h.clock.advance(Duration::days(1));
    h.scored("2026-10-28").await;
    let (_, _, page) = h.get("/yesterday", Some(&cookie)).await;
    assert!(
        page.contains(&format!("🚪 #3  {}", "⬜".repeat(items.len()))),
        "{page}"
    );

    // Every item settled on our answer, except whatever's back today, which
    // stays pending whether it's gold or a carry-over.
    for item in &items {
        let mine = valid_answer(item);
        decide(&h, item, Some(&mine), json!({ mine.clone(): 1.0 })).await;
    }
    let gold = items
        .iter()
        .position(|i| i.item.gold_answer().is_some())
        .unwrap();
    let plain = items
        .iter()
        .position(|i| i.item.gold_answer().is_none())
        .unwrap();
    dorman_testkit::publish(
        &h.pool,
        day("2026-10-28"),
        &[
            items[gold].knock_key.clone(),
            items[plain].knock_key.clone(),
        ],
    )
    .await;
    let expected: String = (0..items.len())
        .map(|n| {
            if n == gold || n == plain {
                '⬜'
            } else {
                '🟩'
            }
        })
        .collect();
    let (_, _, page) = h.get("/yesterday", Some(&cookie)).await;
    assert!(
        page.contains(&format!("🚪 #3  {expected}")),
        "want {expected}: {page}"
    );
}

#[sqlx::test(migrations = "../db/migrations")]
async fn yesterday_cant_tell_gold_from_the_rest(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let cookie = h.play_all(&items).await;
    for item in &items {
        let mine = valid_answer(item);
        decide(
            &h,
            item,
            Some(&mine),
            json!({ mine.clone(): 3.0, "x": 1.0 }),
        )
        .await;
    }
    // A gold item and a plain one are both back today.
    let gold = items
        .iter()
        .find(|i| i.item.gold_answer().is_some())
        .unwrap();
    let plain = items
        .iter()
        .find(|i| i.item.gold_answer().is_none())
        .unwrap();
    h.clock.advance(Duration::days(1));
    h.scored("2026-10-28").await;
    dorman_testkit::publish(
        &h.pool,
        day("2026-10-28"),
        &[gold.knock_key.clone(), plain.knock_key.clone()],
    )
    .await;
    let expected: Vec<String> = items
        .iter()
        .filter(|i| i.id != gold.id && i.id != plain.id)
        .map(|i| i.id.to_string())
        .collect();

    for viewer in [Some(cookie.as_str()), None] {
        let (_, _, page) = h.get("/yesterday", viewer).await;
        assert_eq!(
            listed(&page),
            expected,
            "yesterday's set minus what's back today"
        );
        // Every row reads alike: a settled verdict and bars for a player who
        // answered, nothing for anyone else.
        let rows = expected.len();
        match viewer {
            Some(_) => {
                assert_eq!(page.matches("The crowd settled on:").count(), rows);
                assert_eq!(page.matches(r#"<ul class="bars">"#).count(), rows);
                assert!(page.contains("75%") && page.contains("25%"));
            }
            None => {
                assert_eq!(
                    page.matches(&escaped("You didn't answer this one")).count(),
                    rows
                );
                assert!(!page.contains(r#"<ul class="bars">"#));
                assert!(!page.contains("settled on"));
            }
        }
    }
}

#[sqlx::test(migrations = "../db/migrations")]
async fn yesterday_waits_for_the_night_run_and_never_names_a_raw_answer(pool: PgPool) {
    let h = Harness::new(pool);
    let (_, _, page) = h.get("/yesterday", None).await;
    assert!(page.contains("There was no game yesterday."));

    let items = seed_day(&h.pool, day(TODAY)).await;
    let cookie = h.play_all(&items).await;
    let mine = valid_answer(&items[1]);
    decide(
        &h,
        &items[1],
        Some(&mine),
        json!({ mine.clone(): 3.0, "zzz-unknown-key": 1.0 }),
    )
    .await;
    h.verdict(&items[3], "disputed", None, json!({ "x": 1.0 }))
        .await;
    decide(&h, &items[4], None, json!({})).await;

    h.clock.advance(Duration::days(1));
    let (_, _, page) = h.get("/yesterday", Some(&cookie)).await;
    assert!(page.contains("Results come out the morning after each game"));
    assert!(listed(&page).is_empty());

    h.scored("2026-10-28").await;
    let (_, _, page) = h.get("/yesterday", Some(&cookie)).await;
    assert!(page.contains("Day 3"), "yesterday's number");
    assert_eq!(listed(&page).len(), items.len());
    let label = dorman_api::views::choice_label(&items[1].item, &mine);
    assert!(page.contains(&format!("The crowd settled on: {}.", escaped(&label))));
    assert!(
        page.contains("Something else"),
        "an unknown answer gets no raw name"
    );
    assert!(!page.contains("zzz-unknown-key"));
    assert!(page.contains("Too close to call"));
    assert!(page.contains("Not enough votes yet"));
    assert!(page.contains("No votes were counted for this one."));
}

#[sqlx::test(migrations = "../db/migrations")]
async fn pair_replies_are_shuffled_and_read_as_letters_everywhere(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let pair = items
        .iter()
        .find(|i| matches!(i.item, BatchItem::Pair(_)))
        .unwrap();
    let BatchItem::Pair(p) = &pair.item else {
        unreachable!()
    };
    let order = dorman_api::views::reply_order(p);
    let (a, b) = (order[0], order[1]);

    // The card offers them in that order.
    let cookie = h
        .play_all(&items[..items.iter().position(|i| i.id == pair.id).unwrap()])
        .await;
    let (_, _, page) = h.get("/", Some(&cookie)).await;
    assert!(shows_card(&page, pair));
    let (at_a, at_b) = (
        page.find(&format!(r#"value="{}""#, a.knock_key)).unwrap(),
        page.find(&format!(r#"value="{}""#, b.knock_key)).unwrap(),
    );
    assert!(at_a < at_b);

    // The live split and the reveal both call them by those letters.
    let (_, _, body) = h.vote(Some(&cookie), vote_body(pair, &b.knock_key)).await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["split"][0]["label"], "Reply B");
    h.play_all_as(Some(cookie.clone()), &items).await;
    decide(
        &h,
        pair,
        Some(&b.knock_key),
        json!({ a.knock_key.clone(): 1.0, b.knock_key.clone(): 2.0, "both_bad": 1.0 }),
    )
    .await;
    h.clock.advance(Duration::days(1));
    h.scored("2026-10-28").await;
    let (_, _, page) = h.get("/yesterday", Some(&cookie)).await;
    let reply = escaped(&b.text);
    assert!(
        page.contains(&format!(
            "The crowd settled on: B: \u{201c}{reply}\u{201d}."
        )),
        "{page}"
    );
    assert!(page.contains("Both bad"));
    for reply in &p.replies {
        assert!(
            !page.contains(&format!(">{}<", reply.knock_key)),
            "a reply's key is shown as text"
        );
    }
}

// -- overtime ---------------------------------------------------------------------

/// Two days of the example batch: half of it yesterday, the rest today.
async fn yesterday_and_today(h: &Harness) -> (Vec<StoredItem>, Vec<StoredItem>) {
    let batch = dorman_testkit::load_example(&h.pool).await;
    let keys: Vec<String> = batch
        .items
        .iter()
        .map(|i| i.knock_key().to_string())
        .collect();
    let past = dorman_testkit::publish(&h.pool, day("2026-10-26"), &keys[..3]).await;
    let today = dorman_testkit::publish(&h.pool, day(TODAY), &keys[3..]).await;
    (past, today)
}

fn overtime_body(item: &StoredItem) -> Value {
    let mut body = vote_body(item, &valid_answer(item));
    body["overtime"] = json!(true);
    body
}

#[sqlx::test(migrations = "../db/migrations")]
async fn overtime_opens_only_once_today_is_done(pool: PgPool) {
    let h = Harness::new(pool);
    let (past, today) = yesterday_and_today(&h).await;
    let see_other = |r: &(StatusCode, Option<String>, String)| r.0 == StatusCode::SEE_OTHER;
    assert!(see_other(&h.get("/overtime", None).await));
    let (status, cookie, _) = h.vote(None, overtime_body(&past[0])).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no player, no Overtime");
    assert!(cookie.is_none());

    let cookie = h.play_all(&today[..1]).await;
    assert!(see_other(&h.get("/overtime", Some(&cookie)).await));
    let (status, _, _) = h.vote(Some(&cookie), overtime_body(&past[0])).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "today isn't finished");

    h.play_all_as(Some(cookie.clone()), &today).await;
    let (status, _, page) = h.get("/overtime", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("<h1>Overtime</h1>"));
    assert!(page.contains(r#"<input type="hidden" name="overtime" value="1">"#));
    let (_, _, results) = h.get("/", Some(&cookie)).await;
    assert!(results.contains(r#"<a class="overtime" href="/overtime">"#));
}

#[sqlx::test(migrations = "../db/migrations")]
async fn overtime_serves_each_past_item_once_and_never_todays(pool: PgPool) {
    let h = Harness::new(pool);
    let (past, today) = yesterday_and_today(&h).await;
    let cookie = h.play_all(&today).await;

    // Today's items aren't Overtime items, and Overtime items aren't today's.
    let (status, _, _) = h.vote(Some(&cookie), overtime_body(&today[0])).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = h
        .vote(Some(&cookie), vote_body(&past[0], &valid_answer(&past[0])))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let mut served = Vec::new();
    loop {
        let (_, _, page) = h.get("/overtime", Some(&cookie)).await;
        let Some(item) = past.iter().find(|item| shows_card(&page, item)) else {
            assert!(
                page.contains(&escaped("You've answered everything there is for now.")),
                "{page}"
            );
            break;
        };
        assert!(!today.iter().any(|t| shows_card(&page, t)));
        assert!(!served.contains(&item.id), "served twice");
        served.push(item.id);
        // Half by fetch, half by form post.
        if served.len() % 2 == 1 {
            let (status, _, body) = h.vote(Some(&cookie), overtime_body(item)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                serde_json::from_str::<Value>(&body).unwrap()["remaining"],
                Value::Null
            );
        } else {
            let (id, answer, token) =
                (item.id.to_string(), valid_answer(item), h.turnstile.issue());
            let response = h
                .form_vote(
                    Some(&cookie),
                    &[
                        ("item_id", &id),
                        ("answer", &answer),
                        ("shown_at", "0"),
                        ("overtime", "1"),
                        ("cf-turnstile-response", &token),
                    ],
                )
                .await;
            assert_eq!(
                response.headers()[header::LOCATION],
                format!("/overtime?after={id}")
            );
            let (_, _, page) = h.get(&format!("/overtime?after={id}"), Some(&cookie)).await;
            assert!(page.contains(r#"<a class="next" href="/overtime">Next card</a>"#));
        }
        let (status, _, _) = h.vote(Some(&cookie), overtime_body(item)).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "an answered item is done with"
        );
    }
    assert_eq!(served.len(), past.len());
    let days: Vec<Option<chrono::NaiveDate>> =
        sqlx::query_scalar("SELECT day FROM votes WHERE item_id = ANY ($1)")
            .bind(served)
            .fetch_all(&h.pool)
            .await
            .unwrap();
    assert!(
        days.iter().all(Option::is_none),
        "Overtime answers belong to no day"
    );
}

// -- ranks, the leaderboard and handles ---------------------------------------------

#[sqlx::test(migrations = "../db/migrations")]
async fn the_results_show_a_rank(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let cookie = h.play_all(&items).await;
    let (_, _, page) = h.get("/results", Some(&cookie)).await;
    assert!(page.contains("Your rank: <strong>Rookie</strong>"));
    assert!(page.contains(r#"<form class="handle" method="post" action="/handle">"#));
}

async fn handle_post(h: &Harness, cookie: Option<&str>, handle: &str) -> Response<Body> {
    let mut request = Request::post("/handle")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ORIGIN, ORIGIN);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    let body = serde_urlencoded::to_string([("handle", handle)]).unwrap();
    h.respond(request.body(Body::from(body)).unwrap()).await
}

#[sqlx::test(migrations = "../db/migrations")]
async fn a_handle_is_checked_and_unique(pool: PgPool) {
    let h = Harness::new(pool);
    let items = seed_day(&h.pool, day(TODAY)).await;
    let (ada, bob) = (h.play_all(&items).await, h.play_all(&items).await);
    assert_eq!(
        handle_post(&h, None, "Ada").await.status(),
        StatusCode::FORBIDDEN
    );
    for bad in ["ab", "a b c", "<script>", "-dash", &"x".repeat(21)] {
        assert_eq!(
            handle_post(&h, Some(&ada), bad).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}"
        );
    }
    let response = handle_post(&h, Some(&ada), "  Ada_Lovelace ").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], "/leaderboard");
    assert_eq!(
        handle_post(&h, Some(&bob), "ada_lovelace").await.status(),
        StatusCode::CONFLICT
    );
    let (_, _, page) = h.get("/results", Some(&ada)).await;
    assert!(page.contains("You're <strong>Ada_Lovelace</strong>"));
    // Like any other POST, it needs our Origin.
    let request = Request::post("/handle")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, &bob)
        .body(Body::from("handle=Bobby"))
        .unwrap();
    assert_eq!(h.send(request).await.0, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn the_leaderboard_ranks_this_weeks_named_players(pool: PgPool) {
    let h = Harness::new(pool);
    let (_, _, page) = h.get("/leaderboard", None).await;
    assert!(page.contains("Nobody's qualified yet this week."));
    assert!(
        page.contains(r#"<time datetime="2026-10-26">"#),
        "the week starts on Monday"
    );

    // 24 settled items from this week.
    let mut batch = dorman_testkit::example_batch();
    let template = batch
        .items
        .iter()
        .find(|i| matches!(i, BatchItem::Intent(_)))
        .unwrap()
        .clone();
    batch.batch = "board".into();
    batch.items = (0..24)
        .map(|n| {
            let mut item = template.clone();
            if let BatchItem::Intent(i) = &mut item {
                i.knock_key = format!("board-{n}");
                i.gold_answer = None;
            }
            item
        })
        .collect();
    dorman_testkit::load_pending(&h.pool, &batch).await;
    dorman_testkit::approve(&h.pool, "board").await;
    let keys: Vec<String> = batch
        .items
        .iter()
        .map(|i| i.knock_key().to_string())
        .collect();
    let items = dorman_testkit::publish(&h.pool, day("2026-10-26"), &keys).await;
    for item in &items {
        decide(&h, item, Some(&valid_answer(item)), json!({})).await;
    }
    // (name, answers, right, flagged)
    for (name, answered, right, flagged) in [
        ("Bob", 24, 20, false),
        ("Ada", 24, 20, false),
        ("Cy", 24, 24, false),
        ("Dee", 24, 24, true),
        ("Eve", 19, 19, false),
        ("", 24, 24, false),
    ] {
        let player = dorman_db::create_player(&h.pool).await.unwrap();
        if !name.is_empty() {
            dorman_db::set_handle(&h.pool, player, name).await.unwrap();
        }
        if flagged {
            sqlx::query("UPDATE players SET flags = '{\"same_answer\": 0.3}' WHERE id = $1")
                .bind(player)
                .execute(&h.pool)
                .await
                .unwrap();
        }
        for (n, item) in items.iter().take(answered).enumerate() {
            let answer = if n < right {
                valid_answer(item)
            } else {
                "wrong".to_string()
            };
            dorman_db::record_vote(
                &h.pool,
                player,
                item.id,
                Some(day("2026-10-26")),
                &answer,
                3000,
            )
            .await
            .unwrap();
        }
    }
    let (_, _, page) = h.get("/leaderboard", None).await;
    let rows: Vec<&str> = page.split("<tr><td>").skip(1).collect();
    let names: Vec<&str> = rows
        .iter()
        .map(|r| r.split("</td><td>").nth(1).unwrap())
        .collect();
    assert_eq!(
        names,
        ["Cy", "Ada", "Bob"],
        "flagged, unnamed and too-few are left out"
    );
    assert!(rows[0].contains("<td>100%</td><td>24</td>"));
    assert!(rows[1].contains("<td>83%</td><td>24</td>"));

    // Last week's answers don't count this week.
    h.clock.advance(Duration::days(7));
    let (_, _, page) = h.get("/leaderboard", None).await;
    assert!(page.contains("Nobody's qualified yet this week."));
}
