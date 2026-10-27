# Plan: from scaffold to a finished, fully tested public game

**Starting point (commit `c1b8bbe`):** the KNOCK export/import scripts are
done. Dorman can load a batch, publish a daily set, serve a page, record
votes, and run the nightly scorer. The whole loop has been run end to end once,
by hand.

**"Finished" means:** strangers can play every day on a public URL; abuse
can't poison the data; the nightly pipeline runs without anyone touching it
apart from reviewing batches and disputes; and every layer has automated tests
that run in CI, including against a real Postgres and a real browser.

The original design is in KNOCK at `plans/knock-game.md`. This plan sequences
the remaining work. Languages stay Rust and Python. Browser tests use
Playwright for Python, so there's still no Node toolchain.

---

## Decisions needed from you (these block specific milestones)

| # | Decision | Blocks | Default if you don't care |
|---|---|---|---|
| D1 | Name and domain | M6 (deploy) | Keep "Who's At The Door?" and buy a domain |
| D2 | Hosting budget | M6 | One Fly.io machine plus Fly Postgres, about $10–20/mo |
| D3 | Email provider for magic links | M5 (accounts) | Postmark or Resend free tier; or skip accounts in v1 |
| D4 | Daily reset time zone | M3 | UTC midnight |
| D5 | Who reviews batches and disputes | M4 | Just you |

---

## How the test suite is built

Every milestone below lists its tests. These are the layers they go into.

| Layer | Tool | What it covers | Runs in CI |
|---|---|---|---|
| Rust unit | `cargo test` + `proptest` | `dorman-core`: batch parsing, daily-set picking, options, share card, streak math | yes |
| Rust DB integration | `#[sqlx::test]` (a fresh database per test) | Every query in `dorman-db`: migrations, idempotent loads, daily-set ordering, vote uniqueness, carry-over selection | yes, Postgres service container |
| Rust HTTP integration | `axum` router + `tower::ServiceExt::oneshot`, real test DB | Every route: status codes, cookies, validation, rate limits, Turnstile (stubbed verifier), no leaks in responses | yes |
| Python unit | `pytest` + `hypothesis` | Trust, Dawid-Skene, flags, consensus, brigade detection | yes |
| Scorer simulation | `pytest -m sim` | Simulated crowds (honest, noisy, lazy, trolls, brigades) at several sizes; checks accuracy and troll suppression against fixed thresholds | yes (small); nightly (large) |
| Python DB integration | `pytest` + Postgres | `dorman_scorer.db` load/save, idempotent reruns | yes |
| Contract (cross-repo) | Shared fixtures + a CI job that checks out KNOCK | KNOCK's real export parses in Rust; the scorer's real consensus imports cleanly in KNOCK | yes |
| End-to-end browser | `pytest-playwright` against a running `dorman-api` | Play a full day, refresh, come back tomorrow, share card, streak, mobile viewport, keyboard-only | yes |
| Accessibility | `axe-core` injected through Playwright | No serious or critical violations on every page | yes |
| Security | The HTTP and E2E layers plus a dedicated `security` test module | Cookie tampering, vote stuffing, oversize bodies, CSRF/Origin, CSP headers, the data-leak checks | yes |
| Load | `oha` (Rust CLI) script + a Postgres check | 200 votes/s sustained on one instance, p95 under 100 ms | manual, before launch |

**Coverage gates** (fail CI under these): `cargo llvm-cov` at 85% for `core`
and `db`, 75% for `api`; `pytest-cov` at 90% for `dorman_scorer`.

**Shared test helpers**, built once in M0:
- `crates/testkit`: seeds a database from a fixture batch, makes players with a
  chosen trust and vote history, freezes the clock, and stubs Turnstile.
- `scorer/tests/sim.py`: the crowd simulator, shared by the unit and
  simulation tests.
- `fixtures/`: the real batch from KNOCK (already there), a consensus file
  produced by the scorer, and some adversarial batches (oversized text, markup
  in the text, duplicate keys).

---

## Milestones

Sizes are rough: S is a sitting, M is a few, L is a week of evenings.

### M0. Test foundation (M) — ✅ done

Everything later depends on being able to test against a real database and a
real browser cheaply.

- Add a Postgres 16 service to CI. Move the API's inline JS into
  `static/play.js` so later tests (and the CSP) can treat it as a file.
- Add an injectable clock (`trait Clock`) to the API and scorer. "Today" is
  currently `Utc::now()` everywhere, which makes day-rollover tests
  impossible.
- Build `crates/testkit`, the scorer simulator, and an E2E fixture that starts
  `dorman-api` on a random port against a throwaway database.
- Port the manual smoke test from this session into the first E2E test:
  load the batch, publish a day, vote, check the split.
- Add the coverage gates.

**Done when:** CI runs every layer in the table (each with at least one
real test), and a failing assertion in any layer fails the build.

### M1. Abuse resistance (M) — before anyone outside the house plays

- **Turnstile.** Verify a token when a player is created and on each vote,
  behind a `TurnstileVerifier` trait (real one plus a stub for tests). Fail
  closed: if Cloudflare is down, reject the vote rather than accept it
  unverified.
- **Rate limits, stored in Postgres** so they keep working with more than one
  instance (see the scaling discussion). Per player: votes per minute. Per
  `/24` network: new players per hour. Store only a salted hash of the
  network, kept for 24 hours, so no raw IPs land in the database.
- **Request hardening:** a body size cap, a check that `Origin` matches on
  POST, security headers (strict CSP with no inline script, HSTS,
  `X-Content-Type-Options`, `Referrer-Policy`), and `Cache-Control: no-store`
  on personalized pages.
- **Batch review gate:** loaded batches start as `pending`.
  `dorman-ingest review <batch>` prints every item, and `approve <batch>` makes
  it eligible for daily sets. This is the "human skim" from section 8 of the
  plan.

**Tests:**
- Forged, missing, expired, and replayed Turnstile tokens are rejected.
- A stubbed Cloudflare outage blocks votes.
- Hitting the rate limit gives a 429; it resets once the window passes (using
  the frozen clock).
- A tampered or unsigned cookie gets a fresh player and never someone else's.
- Cross-origin POSTs are refused.
- CSP header present; the E2E run records zero CSP violations.
- Markup in visitor text is escaped. A fixture with
  `<script>alert(1)</script>` as the visitor line renders as text.
- An unapproved batch never makes it into a daily set (property test over
  random pools).

### M2. The core game, finished (L)

What a player sees on a normal day.

- **Flow:** one card at a time with a progress bar, instead of a long list.
  After the last card there's a results screen. Refreshing resumes where you
  left off.
- **Without JavaScript:** each card also works as a plain form POST, so a
  vote doesn't depend on the script loading.
- **Results screen:** your answers next to the live crowd split, plus the
  share card. Each answer reads ⬜ until it settles; next morning the card
  fills in. Copy to clipboard, and the system share sheet on phones.
- **Day number:** the number counts from a fixed launch date, set in config.
- **Streaks:** a `player_days` table (player, day, completed). A streak counts
  consecutive completed days.
- **Yesterday's reveal:** a page showing the full weighted breakdown for
  yesterday's items once the scorer has run.
- **Empty states:** a no-game-today message and a countdown to the next reset.
- **Pages:** an about page explaining what the data is for, in plain words.

**Tests:**
- Day-number and streak math: unit tests plus property tests (a gap breaks
  the streak, the same day counted twice doesn't add to it, behavior around
  time zones per D4).
- The share card never includes item text or answers (a spoiler check).
- HTTP: resume state after reload, the form-POST fallback, and the results
  page before and after the last vote.
- E2E, all three on desktop and phone viewports:
  - play a full day, reload mid-way, finish, copy the card;
  - set the clock forward a day, check the card filled in and the streak is 2;
  - skip a day, check the streak reset.
- Accessibility: every page passes axe. The whole day can be played with the
  keyboard only.
- No leaks: rendered pages and API responses never contain the gold answer,
  the generator's intent, judge scores or `knock_key` (the existing view test,
  extended to every route).

### M3. The scorer, finished (M)

- **Brigade detection** (M0 confirmed the gap: a 40-account brigade settles
  wrong answers today, see `test_a_brigade_of_new_accounts_cannot_settle_a_wrong_answer`):
  flag items that get a burst of votes from accounts
  under 48 hours old all leaning the same way. That item's new-account votes
  then get trust 0 for the scoring pass.
- **Carry-overs:** yesterday's most disputed items go into today's set (the
  query already exists; wire it to real scorer output).
- **A run log:** a `scoring_runs` table with start, end, counts, and the path
  of the export. Runs are idempotent, and a second run on the same day
  overwrites the first cleanly.
- **Weighted split on the reveal page:** the live split stays raw counts,
  since trust isn't known yet; the reveal page shows the weighted one.
- **Settle on the Dawid-Skene posterior, not just the raw weighted share.**
  Found in M0: when honest players are 75% accurate, the right answer can't
  reach the 80% share needed to settle, even though Dawid-Skene is confident.
  Pinned by `test_a_noisy_but_honest_crowd_still_settles_most_items`.
- **`MIN_WEIGHT` and `MIN_SHARE` move into config**, so soft launch can use
  lower thresholds without code changes.
- **Fix in KNOCK:** `scene_for_players()` repeats what the summary already
  says ("holding a pizza bag; carrying a pizza bag").

**Tests:**
- Simulation suite: with 10–40% trolls, 0–3 brigades, and 20–500 players,
  settled-item accuracy stays at 95% or above, and troll trust stays at 0.05
  or below. Assert those thresholds, so a scorer change that degrades them
  fails CI.
- Hypothesis properties:
  - posteriors sum to 1;
  - gold items stay clamped;
  - adding an honest vote never lowers the true label's share;
  - shuffling vote order doesn't change the result.
- DB: running twice gives the same rows; a crash mid-run leaves no partial
  writes (the run happens in one transaction).
- Contract: the scorer's output, run through KNOCK's real
  `import_game_consensus.py` in CI, produces the expected reviews.

### M4. Pair mode polish, Overtime, ranks (M)

- **Pair mode** (already playable): shuffle reply A/B per item so position
  bias can't help. The share card counts pair answers too.
- **Overtime:** endless play after the daily set, served from the backlog,
  favoring items with the least total weight. Its votes go in with
  `day = NULL`, and the scorer weights them by trust earned in dailies, as
  the plan says.
- **Ranks:** Rookie → Doorman → Bouncer → Head of Security. A rank depends
  on accuracy against settled answers plus a minimum number of days played,
  never on volume.
- **Weekly leaderboard:** players with a handle only. Ranked by accuracy
  against settled consensus, with a minimum number of answers to qualify.

**Tests:**
- Overtime never serves an item that's in today's daily set (otherwise it
  would leak spoilers), and never serves an item twice to the same player.
- Rank thresholds at their boundaries.
- The leaderboard excludes players who are flagged or below the minimum, and
  ties break in a stable order.
- E2E: go from the daily set into Overtime and back.
- Simulation: an Overtime-only troll has no effect, since their trust comes
  only from dailies.

### M5. Accounts and data rights (M, optional for v1)

Depends on D3. Without accounts, everything works anonymously; accounts only
add cross-device streaks and a handle for the leaderboard.

- **Email magic link:** a single-use token, valid for 15 minutes, stored
  hashed. Logging in merges the anonymous player into the account, keeping the
  longer streak and the earlier creation date.
- **Data rights:** a "delete my data" button that removes the player and their
  votes, and works without an account. A data export for account holders.
- **Pages:** terms, privacy policy, and a 13+ age check on first visit
  (section 8 of the plan).

**Tests:**
- Tokens are single-use, expire, and are stored hashed (the raw token never
  appears in the database).
- Merging two players with overlapping votes keeps one vote per item.
- After deletion, the player's rows are gone and the scorer runs cleanly
  without them.
- The age check is shown once and remembered.
- E2E: log in on two browser contexts and check the streak is shared.

### M6. Deploy and operate (M)

Depends on D1 and D2.

- **Build:** a multi-stage Dockerfile, with one image holding `dorman-api`,
  `dorman-ingest` and the scorer. Run `cargo-chef` for cached builds.
- **Hosting:** one Fly.io machine plus managed Postgres, with daily backups and
  a tested restore.
- **Scheduled jobs** on a single machine: the scorer at 00:30 UTC, and
  `ingest daily` publishing tomorrow's set at 23:00 UTC. A missed run alerts
  instead of silently skipping a day.
- **Observability:**
  - structured JSON logs with no player IDs at info level;
  - `/healthz` plus a `/readyz` that checks the database;
  - a basic metrics endpoint (votes/min, 4xx/5xx, rate-limit hits);
  - an uptime check.
- **Moving files:** a runbook for getting files between KNOCK and Dorman
  (`scp` or a private bucket), plus the review steps.

**Tests:**
- CI builds the Docker image and runs the E2E suite against the container
  rather than `cargo run`.
- Restoring a backup into a fresh database and running migrations works.
- The load test hits its target (200 votes/s, p95 under 100 ms) on a
  machine the same size as production; record the result in the runbook.

### M7. Launch (S)

- **Soft launch to friends** with gold-heavy dailies (5 gold, 4 new, 1
  carry-over) and lower settle thresholds, to build up trusted players.
  Run the scorer with `--profile soft-launch` (or
  `DORMAN_SCORER_PROFILE=soft-launch`): it lowers the thresholds and turns
  brigade detection off. Under the default profile, a launch-day crowd is all
  accounts under 48 hours old, so it would be held back as a brigade and every
  item would end up disputed. Switch back to the default profile before the
  public launch.
- **Watch for a week:** settle rate, dispute rate, troll flags, and how long
  your review queue gets.
- **Switch to the normal mix** (6/3/1) and the normal thresholds, then share
  it publicly.

**Done when:** a week of unattended daily runs, with your only manual work
being batch approvals and disputed items.

### Later (not part of "finished")

Phase 5 from the design, "Don't Open The Door" red-teaming against a hosted
model. It needs a budget (D2) and its own safety review, so it gets its own
plan.

---

## Order and dependencies

```
M0 ──▶ M1 ──▶ M2 ──▶ M3 ──▶ M4
               │             │
               └──▶ M5 (optional, any time after M2)
                             ▼
                    M6 ──▶ M7
```

M0 and M1 are non-negotiable before anyone outside the house plays. M5 can
be skipped for launch if you don't want accounts yet. Each milestone lands as
its own PR, with green CI and the coverage gates passing.

---

## Working in parallel

Milestones are split into tracks that can run at the same time, each on its
own branch with its own PR into `main`. A coordinator session reviews and
merges.

| Track | Milestones, in order | Owns | Must not touch |
|---|---|---|---|
| **A: API and page** | M1 → M2 → M4 | `crates/api`, `crates/core`, `crates/db/src`, `crates/ingest`, `e2e/test_play.py`, templates and static files | `scorer/` (except reading it) |
| **B: scorer** | M3, plus the KNOCK `scene_for_players()` fix | `scorer/`, `e2e/test_contract.py`, and in KNOCK `scripts/export_game_batch.py` and its tests | `crates/api`, templates, static files |
| **C: deploy prep** | M6 minus the actual deploy | `Dockerfile`, `fly.toml`, `.github/workflows/` (new jobs only), `docs/runbook.md` | application code |

**Migration numbers are reserved per track**, so two branches never both add
`0002_*.sql`:
- Track A: `0002`–`0009`
- Track B: `0010`–`0019`
- Track C: `0020`–`0029` (it shouldn't need any)

**Shared files.** `crates/db/src/lib.rs` (Track B may add queries the scorer
needs) and `crates/testkit` are shared. Add to them; don't restructure them. If
a track has to change something another track owns, it says so in its PR
description so the coordinator can sequence the merges.

**Every PR:** starts from the latest `main`; keeps CI green (fmt, clippy, every
test suite, coverage gates); adds tests for everything it builds, in the
layers named in that milestone; and removes a strict-xfail marker in the same
PR as the fix it describes.
