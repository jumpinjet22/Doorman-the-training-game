"""The file contract between KNOCK and Dorman, using both sides' real code:

KNOCK export_game_batch.py -> dorman-ingest -> votes -> dorman-scorer
-> KNOCK import_game_consensus.py

Needs KNOCK checked out at KNOCK_PATH and importable (`pip install -e`), plus
DATABASE_URL. In CI the contract job does both.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import uuid
from datetime import UTC, datetime, timedelta
from pathlib import Path

import psycopg
import pytest

from conftest import FIXTURE_BATCH, drop_database, fresh_database, ingest
from dorman_scorer.__main__ import main as run_scorer

pytestmark = pytest.mark.contract
DAY = "2026-10-27"

SCENARIOS = [
    ("Hi, I've got a package for you", "delivery", "Thanks, please leave it by the door."),
    ("Hi, I've got a package for you", "delivery", "Nobody's home, just leave it."),
    ("Pizza for Sam?", "food_delivery", "Thanks! Please leave it on the bench."),
    (
        "We're out talking to neighbors about the election",
        "political_soliciting",
        "Thanks, but we're not interested.",
    ),
    ("yeah I'll call you back in five", "not_addressed", "Hello? Can I help you?"),
]


@pytest.fixture(scope="module")
def knock() -> Path:
    path = os.environ.get("KNOCK_PATH")
    if not path:
        pytest.skip("KNOCK_PATH not set")
    pytest.importorskip("knock")
    return Path(path)


def _regular(conn: psycopg.Connection) -> uuid.UUID:
    player = uuid.uuid4()
    conn.execute(
        "INSERT INTO players (id, created_at) VALUES (%s, now() - interval '30 days')", (player,)
    )
    return player


def _synthetic_log(tmp: Path) -> tuple[Path, Path, dict[str, str]]:
    """A synthetic audit log and manifest written with KNOCK's own classes,
    the way generate_training_data.py writes them. Returns (log, reviews,
    text -> example_key for the approved gold entry)."""
    from knock.core.audit import AuditEntry, JSONLAuditLog
    from knock.core.training import TrainingReview, TrainingReviewStore, example_key

    log = JSONLAuditLog(tmp / "synthetic.jsonl")
    reviews = TrainingReviewStore(tmp / "reviews.json")
    manifest = []
    keys: dict[str, str] = {}
    start = datetime(2026, 10, 1, tzinfo=UTC)
    for n, (text, intent, reply) in enumerate(SCENARIOS):
        entry = AuditEntry(
            timestamp=start + timedelta(seconds=n),
            text=text,
            response_text=reply,
            allowed=True,
            reason="llm",
            intent=intent,
        )
        log.record(entry)
        manifest.append(
            {
                "model": "m",
                "scenario": text,
                "intent": intent,
                "reason": "llm",
                "timestamp": entry.timestamp.isoformat(),
                "response_text": reply,
            }
        )
        keys[f"{n}"] = example_key(entry)
    (tmp / "synthetic.manifest.jsonl").write_text(
        "".join(json.dumps(row) + "\n" for row in manifest)
    )
    # A human already approved the first scenario: that makes it gold.
    reviews.set(keys["0"], TrainingReview(status="approved"))
    return log.path, reviews.path, keys


def test_a_batch_round_trips_into_training_reviews(knock, bin_dir, base_db_url, tmp_path) -> None:
    log, reviews, keys = _synthetic_log(tmp_path)
    batch_path = tmp_path / "batch.json"
    subprocess.run(
        [
            sys.executable,
            str(knock / "scripts" / "export_game_batch.py"),
            "--audit-log",
            str(log),
            "--out",
            str(batch_path),
            "--reviews",
            str(reviews),
            "--metadata-store",
            str(tmp_path / "meta.json"),
            "--batch",
            "contract",
        ],
        check=True,
    )
    batch = json.loads(batch_path.read_text())
    target = next(i for i in batch["items"] if i["knock_key"] == keys["4"])

    name, db_url = fresh_database(base_db_url)
    try:
        ingest(bin_dir, db_url, "migrate")
        out = ingest(bin_dir, db_url, "load", str(batch_path))
        assert f"{len(batch['items'])} new items" in out
        ingest(bin_dir, db_url, "daily", "--day", DAY)

        # Trusted regulars (accounts a month old, so not a brigade): right on
        # gold, all agreeing the last line isn't for the door.
        with psycopg.connect(db_url) as conn:
            items = dict(conn.execute("SELECT knock_key, id FROM items").fetchall())
            gold = conn.execute(
                "SELECT id, gold_answer FROM items WHERE gold_answer IS NOT NULL"
            ).fetchall()
            assert gold, "the approved entry should arrive as gold"
            for _ in range(60):
                player = _regular(conn)
                for item_id, answer in [*gold, (items[keys["4"]], "not_addressed")]:
                    conn.execute(
                        "INSERT INTO votes (player_id, item_id, day, answer, ms_to_answer)"
                        " VALUES (%s, %s, %s, %s, 4000)",
                        (player, item_id, DAY, answer),
                    )

        assert (
            run_scorer(["--database-url", db_url, "--out-dir", str(tmp_path), "--today", DAY]) == 0
        )
    finally:
        drop_database(base_db_url, name)

    consensus = tmp_path / f"consensus-{DAY}.json"
    subprocess.run(
        [
            sys.executable,
            str(knock / "scripts" / "import_game_consensus.py"),
            str(consensus),
            "--audit-log",
            str(log),
            "--reviews",
            str(reviews),
        ],
        check=True,
    )
    written = json.loads(reviews.read_text())
    assert written[target["knock_key"]]["status"] == "approved"
    assert "crowd settled" in written[target["knock_key"]]["comment"]
    assert written[keys["0"]] == {"status": "approved"}, "the human review is untouched"


def test_tomorrows_carry_over_is_todays_least_settled_item(bin_dir, base_db_url, tmp_path) -> None:
    """The scorer's consensus rows drive `dorman-ingest daily`'s carry-over:
    a settled item never comes back, and the unsettled item with the weakest
    lead does."""
    batch = json.loads(FIXTURE_BATCH.read_text())
    open_items = [i for i in batch["items"] if not i["gold_answer"]]
    assert len(open_items) == 3
    settled, leaning, split = (i["knock_key"] for i in open_items)
    # Agreeing with the generator, so the judge guard stays out of it.
    right = {i["knock_key"]: i["generator_intent"] for i in open_items}
    tomorrow = "2026-10-28"

    name, db_url = fresh_database(base_db_url)
    try:
        ingest(bin_dir, db_url, "migrate")
        ingest(bin_dir, db_url, "load", str(FIXTURE_BATCH))
        ingest(bin_dir, db_url, "daily", "--day", DAY)
        with psycopg.connect(db_url) as conn:
            items = dict(conn.execute("SELECT knock_key, id FROM items").fetchall())
            gold = conn.execute(
                "SELECT id, gold_answer FROM items WHERE gold_answer IS NOT NULL"
            ).fetchall()
            for n in range(20):
                ballot = [
                    (items[settled], right[settled]),
                    # 15 to 5: a clear lead, but short of settling.
                    (items[leaning], right[leaning] if n < 15 else "other"),
                    # 11 to 9: barely a lead at all.
                    (items[split], right[split] if n < 11 else "other"),
                ]
                player = _regular(conn)
                for item_id, answer in [*gold, *ballot]:
                    conn.execute(
                        "INSERT INTO votes (player_id, item_id, day, answer, ms_to_answer)"
                        " VALUES (%s, %s, %s, %s, 4000)",
                        (player, item_id, DAY, answer),
                    )

        args = ["--database-url", db_url, "--out-dir", str(tmp_path), "--today", DAY]
        # A share of 0.75 needs Dawid-Skene to be sure; keep this about ordering.
        assert run_scorer([*args, "--min-posterior", "1.01"]) == 0
        out = ingest(bin_dir, db_url, "daily", "--day", tomorrow)
        assert "2 carry-over" in out
        with psycopg.connect(db_url) as conn:
            statuses = dict(
                conn.execute(
                    "SELECT i.knock_key, c.status FROM consensus c JOIN items i ON i.id = c.item_id"
                ).fetchall()
            )
            [(keys,)] = conn.execute(
                "SELECT array_agg(i.knock_key) FROM daily_sets d"
                " JOIN items i ON i.id = ANY (d.item_ids) WHERE d.day = %s",
                (tomorrow,),
            ).fetchall()
    finally:
        drop_database(base_db_url, name)

    assert statuses == {settled: "settled", leaning: "open", split: "open"}
    assert split in keys
    assert settled not in keys and leaning not in keys
