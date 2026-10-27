# Dorman: Who's At The Door?

A free daily web game that crowd-reviews [KNOCK](https://github.com/jumpinjet22/KNOCK)'s
**synthetic** training data. Players guess why a visitor is at the door, or pick
the better of two replies. Gold questions and Dawid-Skene scoring quietly turn
trolls' votes down. Each night the crowd's verdicts go back to KNOCK as training
reviews.

The design lives in KNOCK at `plans/knock-game.md`. This repo is the game.

## How data moves

```
KNOCK scratch box                       Dorman
scripts/export_game_batch.py ─ batch.json ─▶ dorman-ingest load
                                           dorman-ingest daily    (publishes a day's set)
                                           dorman-api             (players vote)
                                           dorman-scorer          (nightly: trust + consensus)
scripts/import_game_consensus.py ◀─ consensus-<date>.json ─┘
```

Files only, moved by a person, in both directions. KNOCK never talks to the
game and the game never talks to KNOCK. Batches only ever come from a synthetic
audit log; KNOCK's export script refuses anything else.

## Layout

| Path | What |
|---|---|
| `crates/core` | Batch format, daily-set picking, intent options, share card. Pure, no IO. |
| `crates/db` | Postgres queries and `migrations/`. |
| `crates/ingest` | `dorman-ingest`: `migrate`, `load <batch.json>`, `daily [--day]`. |
| `crates/api` | `dorman-api`: Axum server, renders pages itself (askama + vanilla JS). |
| `scorer/` | `dorman-scorer`: Python nightly job. Gold trust, Dawid-Skene, behavior flags, consensus. |
| `fixtures/batch.example.json` | A real batch from KNOCK's export script, used by tests. |

## Running it locally

```bash
export DATABASE_URL=postgres://dorman@127.0.0.1:5432/dorman

cargo run -p dorman-ingest -- migrate
cargo run -p dorman-ingest -- load fixtures/batch.example.json
cargo run -p dorman-ingest -- daily --day "$(date -u +%F)"

DORMAN_COOKIE_SECRET=$(openssl rand -hex 32) DORMAN_SECURE_COOKIES=false \
  cargo run -p dorman-api          # http://127.0.0.1:8080

cd scorer && pip install -e ".[dev]" && python -m dorman_scorer --out-dir exports/
```

Then, on the KNOCK box:

```bash
python scripts/import_game_consensus.py exports/consensus-<date>.json \
  --audit-log <the synthetic log> --dry-run
```

## Tests

Most suites need Postgres. Point `DATABASE_URL` at a role that can create
databases; every test gets its own throwaway database.

```bash
export DATABASE_URL=postgres://dorman@127.0.0.1:5432/dorman

cargo test --all-features                     # unit, property, Postgres and HTTP tests
cd scorer && pytest                           # unit, property, simulation, Postgres
cd .. && pip install -r e2e/requirements.txt && playwright install chromium
KNOCK_PATH=../KNOCK pytest e2e                # real browser + KNOCK contract
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
| KNOCK export -> Dorman -> KNOCK import | `e2e/test_contract.py` (needs `KNOCK_PATH`) |

The E2E server is built with the `test-clock` feature, which lets tests set
the time through a file. Production builds never include it.

CI fails under these line-coverage gates: `dorman-core` 85%, `dorman-db` 85%,
`dorman-api` 75%, `dorman_scorer` 90%.

Tests marked `xfail(strict=True)` are known gaps that are planned work. They
fail CI the moment they start passing, so the marker gets removed with the fix.

## Trust and consensus, in short

- A new player's vote counts about 0.1 of a veteran's: `Beta(1, 9)` over their
  gold answers. Gold is mixed into every daily set and looks like everything else.
- Dawid-Skene over the non-gold votes scales that down for players who are
  systematically off. A player needs 5+ non-gold votes before it applies.
- Answering too fast, always picking the same position, or alternating
  positions multiplies trust by 0.3. Players are never told.
- An item **settles** at ≥ 80% weighted share and ≥ 5.0 total weight. If it
  hasn't settled 3 days after it was first shown, it's **disputed** and goes to
  a human. A settled answer that contradicts a confident KNOCK judge is also
  disputed.

Heads-up: 5.0 weight is about 8 regulars with 20 gold answers each, or 50
brand-new players. At soft launch most items will end up disputed. That's
expected; tune `MIN_WEIGHT` in `scorer/dorman_scorer/consensus.py` once real
traffic exists.

## Not done yet (before going public)

- Cloudflare Turnstile on session start and votes, and rate limiting.
- The share card and streaks in the UI (the logic is in `crates/core/src/share.rs`).
- Overtime mode, ranks, leaderboard.
- Brigade detection beyond the judge guard (many new accounts piling onto the same items).
- Terms, privacy policy, 13+ age gate (see section 8 of the plan).
- Switching sqlx to compile-time-checked `query!` macros once CI has Postgres.
