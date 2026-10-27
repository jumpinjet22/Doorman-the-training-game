# Doorman: Who's At The Door?

A free daily web game that harvests labels for training a doorbell
assistant. Players read what a visitor says at the door and guess why
they're there, or pick the better of two replies the assistant could give.
The crowd's answers, weighed by how trustworthy each player has proven to be,
become training data: intent labels for a classifier, and reply preferences
for DPO. Each night the labels are exported and a fresh intent model is
trained, replacing the current one only if it does better.

Every visitor line is **synthetic**: generated, reviewed by a person, then
played. No real doorbell footage or recordings are ever shown.

Doorman carries KNOCK's branding but is standalone: it makes its own data,
runs its own game and exports its own datasets, and doesn't connect to KNOCK
or any other codebase. Its planned home is `doorman.knock.cam`.

> The code predates the spelling fix, so crates, binaries and settings are
> still named `dorman` (`dorman-api`, `dorman-ingest`, `DORMAN_*`).

## How to play

- **Ten cards a day**, the same for everyone, resetting at midnight UTC.
- **Intent cards:** read what the visitor said (and the scene, if there is
  one) and pick why they're at the door, from four choices.
- **Pair cards:** pick the better of two replies, or say both are bad.
- After each answer you see how everyone else split. The next morning,
  `/yesterday` reveals what the crowd decided, with an explanation.
- Streaks, ranks and a weekly `/leaderboard` reward playing carefully;
  **Overtime** serves extra cards once the day's set is done.

Some cards have a known answer and are used to measure each player. They look
exactly like every other card, so the only way to score well is to answer
honestly.

## What it produces

`dorman-ingest export <dir>` writes what the crowd has decided, for a
training job to pick up. Run it after the scorer's nightly pass.

| File | One row per | Fields |
|---|---|---|
| `intent.jsonl` | labelled intent item | `id`, `text`, `scene`, `label`, `source`, `intended_intent`, `agreement`, `weight`, `votes`, `batch` |
| `dpo.jsonl` | pair item with a preferred reply | `prompt`, `chosen`, `rejected` (loads straight into a DPO trainer), plus `id`, `text`, `scene`, `source`, `agreement`, `weight`, `votes`, `batch` |
| `manifest.json` | export | format and version, when it ran, `scored_through` (latest scoring run), row count and SHA-256 per file, and how many items were skipped and why |

- Gold items are labelled with their known answer (`source: "gold"`).
  Everything else is exported only once the scorer has settled it
  (`source: "crowd"`). Open and disputed items are skipped, and so are pairs
  judged `both_bad`, since there's no reply to prefer.
- Brigade and cohort votes are already zeroed, so they never shape a label.
- `agreement` is the share of trust behind the exported label and `weight` the
  total trust on the item. Filter or weight on them to keep only confident
  labels. Both are null for gold nobody has answered yet.
- Only approved batches are exported. Files are written through a rename, so
  a reader never sees half of one, and the manifest is written last.

**An export holds every gold answer.** Keep it off the web server's static
paths and out of the repo; anyone with it can pass as a perfect player.

### The nightly model

`dorman-trainer` (in `trainer/`) turns an export into an intent classifier:
TF-IDF over words and characters, then logistic regression. It needs no GPU
and trains in seconds. Run it from cron after the scorer:

```bash
dorman-scorer
dorman-ingest export /var/lib/dorman/export
dorman-trainer /var/lib/dorman/export /var/lib/dorman/models
```

- If the export's `intent.jsonl` hasn't changed since the current model was
  trained (same SHA-256), it does nothing.
- Crowd labels need `--min-agreement` (default 0.8) of the trust behind them
  to be trained on, and count in proportion to it. Gold always counts fully.
- Gold is always held out for testing, along with a fixed share of other
  items (`--test-share`, default 0.2). Each item stays on the same side of
  the split every night.
- Each run that trains writes `models/<time>/` (`model.joblib`,
  `metrics.json` with macro-F1 and per-intent precision and recall, and the
  manifest it learned from). `models/current` moves to it only if it scores
  at least as well as the current model **on the same held-out items**.
- Too little data (`--min-train`, default 20 rows, at least two intents) is
  a normal night. Only real errors exit non-zero, so cron mail means
  something broke.

`model.joblib` is a pickle: only load files this job wrote. DPO needs a GPU
and isn't trained here; take `dpo.jsonl` to a hosted fine-tuning service or a
rented GPU.

## How data moves

```
a dorman-batch file ─▶ dorman-ingest load      (pending review)
                       dorman-ingest review/approve
                       dorman-ingest daily      (publishes a day's set)
                       dorman-api               (players vote)
                       dorman-scorer            (nightly: trust + consensus, saved in Postgres)
                       dorman-ingest export     (training data: intent + DPO JSONL)
```

A batch is a `dorman-batch` v1 JSON file (see `fixtures/batch.example.json`
and `crates/core/src/batch.rs`). Everything downstream reads and writes
Postgres; nothing is exchanged with another system.

## Running it locally

```bash
export DATABASE_URL=postgres://dorman@127.0.0.1:5432/dorman

cargo run -p dorman-ingest -- migrate
cargo run -p dorman-ingest -- load fixtures/batch.example.json
cargo run -p dorman-ingest -- review example-1     # skim every item...
cargo run -p dorman-ingest -- approve example-1    # ...then let it into daily sets
cargo run -p dorman-ingest -- daily --day "$(date -u +%F)"

# Cloudflare's always-pass Turnstile test keys; never use them in production.
# The server refuses them unless the public origin is http:// on localhost,
# 127.0.0.1 or [::1].
DORMAN_COOKIE_SECRET=$(openssl rand -hex 32) \
DORMAN_LAUNCH_DATE="$(date -u +%F)" \
DORMAN_TURNSTILE_SITE_KEY=1x00000000000000000000AA \
DORMAN_TURNSTILE_SECRET=1x0000000000000000000000000000000AA \
  cargo run -p dorman-api          # http://127.0.0.1:8080

cd scorer && pip install -e ".[dev]" && python -m dorman_scorer
```

A loaded batch is `pending` until someone runs `approve`; only approved
batches feed daily sets.

A batch item may carry an optional `explanation` (up to 400 characters). It is
shown on `/yesterday`, to players who answered the item, once the item is out
of play: an explanation names the answer, so one shown while the card could
still come round (gold runs on for up to 5 days) would leak a gold answer.
Players may also say how sure they are (`unsure`, `fairly`, `certain`); that
is stored on the vote and summarised on their results page, and the scorer
ignores it.

## Configuration

`dorman-api` settings, as environment variables:

| Variable | Default | What |
|---|---|---|
| `DORMAN_COOKIE_SECRET` or `DORMAN_COOKIE_SECRET_FILE` | one required | 32+ random bytes (`openssl rand -hex 32`); signs the player cookie and salts network hashes. Short or repetitive values are refused. Prefer the file form (e.g. a mounted secret); there is deliberately no documented CLI flag, since flags show in the process list |
| `DORMAN_LAUNCH_DATE` | required | day 1 of the game (`YYYY-MM-DD`); day numbers on the page and share card count from it |
| `DORMAN_TURNSTILE_SITE_KEY`, `DORMAN_TURNSTILE_SECRET` | required | Cloudflare Turnstile. If Cloudflare can't be reached, votes are refused. A token passes only if Cloudflare says it was solved on the host of `DORMAN_PUBLIC_ORIGIN` for the `vote` action. Cloudflare's test keys are refused unless the origin is `http://` on localhost, 127.0.0.1 or [::1] |
| `DORMAN_PUBLIC_ORIGIN` | `http://<bind>` | e.g. `https://example.com`; POSTs from any other `Origin` get 403. HSTS is sent only when this is https |
| `DORMAN_CLIENT_IP_HEADER` | unset (TCP peer) | e.g. `Fly-Client-IP`; set only behind a proxy that overwrites it |
| `DORMAN_VOTES_PER_MINUTE` | 30 | per player |
| `DORMAN_NEW_PLAYERS_PER_HOUR` | 20 | per /24 (IPv4) or /48 (IPv6), stored as a salted hash for 24 hours |

Behind a proxy or load balancer, set `DORMAN_CLIENT_IP_HEADER`: without it,
every client shares the proxy's address, so the whole site gets one network's
allowance of new players (20 an hour by default). A request with no usable
address at all goes into one shared `unknown` bucket and is logged at error
level each time, so a misconfigured header shows up in the logs at once.

The limit is per network on purpose: it makes account farms slow, at the cost
that a school or a carrier's NAT shares one allowance. Only players actually
created count against it, so refused or failed attempts don't use it up, and
a vote that's over a rate limit is refused before its single-use Turnstile
token is spent.

### Rotating the cookie secret

The cookie-signing key and the network-hash salt are derived from the cookie
secret independently (SHA-512 of the secret for the key; SHA-256 of a
`dorman network salt` label and the secret for the salt), so leaking one
reveals neither the other nor the secret. Both change when the secret does:

- every player's cookie stops verifying, so everyone becomes a new anonymous
  player once (their past votes stay, but their streak and results page start
  over);
- every network's new-player window resets, since the stored hashes no
  longer match.

So rotate only when the secret may have leaked, ideally at a quiet hour:
write a new `openssl rand -hex 32` to the secret file (or variable) and
restart every instance together, since instances with different secrets
reject each other's cookies.

## Layout

| Path | What |
|---|---|
| `crates/core` | Batch format, daily-set picking, intent options, share card. Pure, no IO. |
| `crates/db` | Postgres queries and `migrations/`. |
| `crates/ingest` | `dorman-ingest`: `migrate`, `load <batch.json>`, `review`/`approve <batch>`, `daily [--day] [--gold-showings N]` (gold retires after N sets, default 5; warns when it runs low), `clear-handle <handle>`, `export <dir>` (training data, below). |
| `crates/api` | `dorman-api`: Axum server, renders pages itself (askama + vanilla JS): today's game one card at a time, results and share card, Overtime, ranks, `/leaderboard`, `/yesterday`, `/about`. |
| `scorer/` | `dorman-scorer`: Python nightly job. Gold trust, Dawid-Skene, behavior flags, consensus. |
| `trainer/` | `dorman-trainer`: Python nightly job. Trains the intent model from an export and promotes it if it's better. |
| `fixtures/batch.example.json` | An example `dorman-batch` file, used by tests. |

## Tests

Most suites need Postgres. Point `DATABASE_URL` at a role that can create
databases; every test gets its own throwaway database.

```bash
export DATABASE_URL=postgres://dorman@127.0.0.1:5432/dorman

cargo test --all-features                # unit, property, Postgres and HTTP tests
(cd scorer && pytest)                    # unit, property, simulation, Postgres
(cd scorer && pytest -m slow)            # large simulations (nightly)
(cd trainer && pytest)                   # trains on a small synthetic export
pip install -r e2e/requirements.txt && playwright install chromium
pytest e2e                               # real browser
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
`dorman-api` 75%, `dorman_scorer` 90%, `dorman_trainer` 90%.

Tests marked `xfail(strict=True)` are known gaps that are planned work. They
fail CI the moment they start passing, so the marker gets removed with the fix.

## Keeping the crowd honest

- A new player's vote counts about 0.1 of a veteran's: `Beta(1, 9)` over their
  gold answers. Gold is mixed into every daily set and looks like everything else,
  down to the reveal page and share card: the scorer decides gold's row from the
  crowd's votes with the same rule as any other item's, never from the known
  answer. Gold runs on consecutive days only and retires after 5 daily sets or
  the first day it's left out, whichever comes first, so the reveal the day
  after its last showing can never be reused on it. Nothing about an item
  still in play (a record's "right" count included) is shown before it leaves.
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
- **Cohorts:** accounts created within 72 hours of each other that give the
  same answer *against* everyone else's most common one on 5+ non-gold items
  (and agree on 90%+ of everything they both answered) are linked; agreeing on
  the right answer never links anyone. 8+ linked accounts are a cohort,
  whatever their age, and on any item where a cohort's answer isn't the clear
  choice of everyone else, its votes count for zero.
  That catches farms that age their accounts first or keep under 8 per item.
- **Answer timing** is measured by the server: each card is rendered with a
  signed token naming the item, the player and when it was served, and the
  vote hands it back. Whatever the client says about its own timing is
  ignored; a missing or forged token counts as no time at all.
- An item **settles** with ≥ 5.0 total weight and either ≥ 80% weighted share,
  or a Dawid-Skene posterior ≥ 0.95 with ≥ 60% weighted share. If it hasn't
  settled 3 days after it was first shown, it's **disputed** and goes to a
  human.
- Each run is one transaction and is logged in `scoring_runs`; rerunning a
  day replaces it. A run with `new_account_hours` at 0 (soft launch) logs a
  warning and records it in `scoring_runs.warnings`. `consensus.split` holds
  the weighted split per answer for the reveal page.

Every threshold is configurable: `--profile soft-launch` (weight 2.0, share
0.7, brigade detection off, since on launch day every account is new), `DORMAN_MIN_WEIGHT`-style environment variables, or flags; see
`python -m dorman_scorer --help`. 5.0 weight is about 8 regulars with 20 gold
answers each, so use the soft-launch profile until real traffic exists.

## Not done yet (before going public)

- Terms, privacy policy, 13+ age gate (see section 8 of the plan).
- A line on `/about` telling players their answers train a model.
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
