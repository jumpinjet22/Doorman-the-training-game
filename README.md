# Dorman: Who's At The Door?

A free daily web game that crowd-reviews **synthetic** data for fine-tuning a
doorbell-assistant model. Players guess why a visitor is at the door, or pick
the better of two replies. Gold questions and Dawid-Skene scoring quietly turn
trolls' votes down. The crowd's verdicts, with a maintainer's review, become
the training dataset.

This repo is self-contained: it makes its own data, reviews it, and exports
it. It does not depend on any other codebase.

## How data moves

```
a dorman-batch file ─▶ dorman-ingest load      (pending review)
                       dorman-ingest review/approve
                       dorman-ingest daily      (publishes a day's set)
                       dorman-api               (players vote)
                       dorman-scorer            (nightly: trust + consensus, saved in Postgres)
```

A batch is a `dorman-batch` v1 JSON file (see `fixtures/batch.example.json`
and `crates/core/src/batch.rs`). Everything downstream reads and writes
Postgres; nothing is exchanged with another system.

## Layout

| Path | What |
|---|---|
| `crates/core` | Batch format, daily-set picking, intent options, share card. Pure, no IO. |
| `crates/db` | Postgres queries and `migrations/`. |
| `crates/ingest` | `dorman-ingest`: `migrate`, `load <batch.json>`, `review`/`approve <batch>`, `daily [--day] [--gold-showings N]` (gold retires after N sets, default 5; warns when it runs low). |
| `crates/api` | `dorman-api`: Axum server, renders pages itself (askama + vanilla JS): today's game one card at a time, results and share card, Overtime, ranks, `/leaderboard`, `/yesterday`, `/about`. |
| `scorer/` | `dorman-scorer`: Python nightly job. Gold trust, Dawid-Skene, behavior flags, consensus. |
| `fixtures/batch.example.json` | An example `dorman-batch` file, used by tests. |

## Running it locally

```bash
export DATABASE_URL=postgres://dorman@127.0.0.1:5432/dorman

cargo run -p dorman-ingest -- migrate
cargo run -p dorman-ingest -- load fixtures/batch.example.json
cargo run -p dorman-ingest -- review example-1     # skim every item...
cargo run -p dorman-ingest -- approve example-1    # ...then let it into daily sets
cargo run -p dorman-ingest -- daily --day "$(date -u +%F)"

# Cloudflare's always-pass Turnstile test keys; never use them in production.
DORMAN_COOKIE_SECRET=$(openssl rand -hex 32) DORMAN_SECURE_COOKIES=false \
DORMAN_LAUNCH_DATE="$(date -u +%F)" \
DORMAN_TURNSTILE_SITE_KEY=1x00000000000000000000AA \
DORMAN_TURNSTILE_SECRET=1x0000000000000000000000000000000AA \
  cargo run -p dorman-api          # http://127.0.0.1:8080

cd scorer && pip install -e ".[dev]" && python -m dorman_scorer
```

A loaded batch is `pending` until someone runs `approve`; only approved
batches feed daily sets.

A batch item may carry an optional `explanation` (up to 400 characters). It is
shown only after a player has voted, so it never reaches an unanswered card.
Players may also say how sure they are (`unsure`, `fairly`, `certain`); that
is stored on the vote and summarised on their results page, and the scorer
ignores it.

`dorman-api` settings, as environment variables:

| Variable | Default | What |
|---|---|---|
| `DORMAN_COOKIE_SECRET` | required | 32+ bytes; signs the player cookie and salts network hashes |
| `DORMAN_LAUNCH_DATE` | required | day 1 of the game (`YYYY-MM-DD`); day numbers on the page and share card count from it |
| `DORMAN_TURNSTILE_SITE_KEY`, `DORMAN_TURNSTILE_SECRET` | required | Cloudflare Turnstile. If Cloudflare can't be reached, votes are refused |
| `DORMAN_PUBLIC_ORIGIN` | `http://<bind>` | e.g. `https://example.com`; POSTs from any other `Origin` get 403 |
| `DORMAN_CLIENT_IP_HEADER` | unset (TCP peer) | e.g. `Fly-Client-IP`; set only behind a proxy that overwrites it |
| `DORMAN_VOTES_PER_MINUTE` | 30 | per player |
| `DORMAN_NEW_PLAYERS_PER_HOUR` | 20 | per /24 (IPv4) or /48 (IPv6), stored as a salted hash for 24 hours |
| `DORMAN_SECURE_COOKIES` | true | turn off only for local http |

Behind a proxy or load balancer, set `DORMAN_CLIENT_IP_HEADER`: without it,
every client shares the proxy's address, so the whole site gets one network's
allowance of new players (20 an hour by default).

## Tests

Most suites need Postgres. Point `DATABASE_URL` at a role that can create
databases; every test gets its own throwaway database.

```bash
export DATABASE_URL=postgres://dorman@127.0.0.1:5432/dorman

cargo test --all-features                     # unit, property, Postgres and HTTP tests
cd scorer && pytest                           # unit, property, simulation, Postgres
cd scorer && pytest -m slow                   # large simulations (nightly)
cd .. && pip install -r e2e/requirements.txt && playwright install chromium
pytest e2e                                   # real browser
```

| Suite | Where |
|---|---|
| Rust unit + property (`proptest`) | `crates/core/src/*.rs` |
| Postgres queries (`#[sqlx::test]`) | `crates/db/tests/` |
| HTTP routes and security, frozen clock | `crates/api/tests/routes.rs` |
| Shared Rust helpers | `crates/testkit` |
| Scorer unit + `hypothesis` | `scorer/tests/test_scoring.py`, `test_properties.py` |
| Simulated crowds (`-m sim`) | `scorer/tests/sim.py`, `test_simulation.py` |
| Scorer against Postgres (`-m db`) | `scorer/tests/test_db.py` |
| Browser, accessibility (axe), leaks | `e2e/test_play.py` |

The E2E server is built with the `test-clock` feature, which lets tests set
the time through a file and point Turnstile checks at a fake Cloudflare.
Production builds never include it. The browser side gets a fake Turnstile
widget through Playwright routing, so the suite never talks to Cloudflare.

CI fails under these line-coverage gates: `dorman-core` 85%, `dorman-db` 85%,
`dorman-api` 75%, `dorman_scorer` 90%.

Tests marked `xfail(strict=True)` are known gaps that are planned work. They
fail CI the moment they start passing, so the marker gets removed with the fix.

## Trust and consensus, in short

- A new player's vote counts about 0.1 of a veteran's: `Beta(1, 9)` over their
  gold answers. Gold is mixed into every daily set and looks like everything else,
  down to the reveal page and share card (the scorer writes it a settled row like
  any other item's). Each gold item retires after 5 daily sets.
- Dawid-Skene over the non-gold votes scales that down for players who are
  systematically off. A player needs 5+ non-gold votes before it applies.
- Answering too fast, always picking the same position, alternating
  positions, or giving the same answer to over 90% of items multiplies trust
  by 0.3. Players are never told.
- **Brigades:** when 8+ accounts under 48 hours old (at the time they voted)
  pick the same answer on one item, and established players don't clearly
  favor that answer, every new-account vote on that item counts for zero in
  that night's run. With no established votes on the item yet, the burst is
  held back until regulars weigh in.
- An item **settles** with ≥ 5.0 total weight and either ≥ 80% weighted share,
  or a Dawid-Skene posterior ≥ 0.95 with ≥ 60% weighted share. If it hasn't
  settled 3 days after it was first shown, it's **disputed** and goes to a
  human.
- Each run is one transaction and is logged in `scoring_runs`; rerunning a
  day replaces it. `consensus.split` holds the weighted split per answer for
  the reveal page.

Every threshold is configurable: `--profile soft-launch` (weight 2.0, share
0.7, brigade detection off, since on launch day every account is new), `DORMAN_MIN_WEIGHT`-style environment variables, or flags; see
`python -m dorman_scorer --help`. 5.0 weight is about 8 regulars with 20 gold
answers each, so use the soft-launch profile until real traffic exists.

## Not done yet (before going public)

- Terms, privacy policy, 13+ age gate (see section 8 of the plan).
- Switching sqlx to compile-time-checked `query!` macros once CI has Postgres.

## Security

Please report vulnerabilities privately; see [SECURITY.md](SECURITY.md).

## Contributors

Thanks goes to these wonderful people ([emoji key](https://allcontributors.org/docs/en/emoji-key)):

<!-- ALL-CONTRIBUTORS-LIST:START - Do not remove or modify this section -->
<!-- prettier-ignore-start -->
<!-- markdownlint-disable -->

<!-- markdownlint-restore -->
<!-- prettier-ignore-end -->
<!-- ALL-CONTRIBUTORS-LIST:END -->

This project follows the [all-contributors](https://allcontributors.org) specification. Comment `@all-contributors please add @<username> for <contributions>` on an issue or PR to add someone.
