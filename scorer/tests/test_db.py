"""dorman_scorer.db and the nightly entry point, against real Postgres."""

from __future__ import annotations

import json
import uuid
from datetime import date
from pathlib import Path

import psycopg
import pytest

from dorman_scorer import db
from dorman_scorer.__main__ import main

pytestmark = pytest.mark.db

BATCH = json.loads(
    (Path(__file__).resolve().parents[2] / "fixtures" / "batch.example.json").read_text()
)
DAY = date(2026, 10, 27)


def _seed(conn: psycopg.Connection) -> dict[str, str]:
    """Loads the example batch and publishes it for DAY. Returns knock_key -> id."""
    ids: dict[str, str] = {}
    for item in BATCH["items"]:
        payload = dict(item)
        if item["mode"] == "intent":
            others = [i for i in BATCH["intents"] if i != item["generator_intent"]]
            payload["options"] = [item["generator_intent"], *others[:3]]
        item_id = str(uuid.uuid4())
        ids[item["knock_key"]] = item_id
        conn.execute(
            "INSERT INTO items (id, knock_key, mode, payload, gold_answer, batch)"
            " VALUES (%s, %s, %s, %s, %s, %s)",
            (
                item_id,
                item["knock_key"],
                item["mode"],
                json.dumps(payload),
                item["gold_answer"],
                BATCH["batch"],
            ),
        )
    conn.execute(
        "INSERT INTO daily_sets (day, item_ids) VALUES (%s, %s::uuid[])", (DAY, list(ids.values()))
    )
    return ids


def _player(conn: psycopg.Connection, votes: list[tuple[str, str]], ms: int = 4000) -> str:
    player = str(uuid.uuid4())
    conn.execute("INSERT INTO players (id) VALUES (%s)", (player,))
    for item_id, answer in votes:
        conn.execute(
            "INSERT INTO votes (player_id, item_id, day, answer, ms_to_answer)"
            " VALUES (%s, %s, %s, %s, %s)",
            (player, item_id, DAY, answer, ms),
        )
    return player


def _crowd(conn, ids, n=20):
    """n players who get every gold item right and agree on one open item."""
    gold = [(ids[i["knock_key"]], i["gold_answer"]) for i in BATCH["items"] if i["gold_answer"]]
    target = next(i for i in BATCH["items"] if i["mode"] == "intent" and not i["gold_answer"])
    for _ in range(n):
        _player(conn, [*gold, (ids[target["knock_key"]], target["generator_intent"])])
    return target


def test_load_reads_items_votes_and_positions(db_url: str) -> None:
    with psycopg.connect(db_url) as conn:
        ids = _seed(conn)
        pair = next(i for i in BATCH["items"] if i["mode"] == "pair")
        intent = next(i for i in BATCH["items"] if i["mode"] == "intent")
        _player(
            conn,
            [
                (ids[pair["knock_key"]], "both_bad"),
                (ids[intent["knock_key"]], intent["generator_intent"]),
            ],
        )
        items, votes = db.load(conn)

    assert len(items) == len(BATCH["items"])
    loaded_pair = next(i for i in items if i.mode == "pair")
    assert loaded_pair.reply_keys == tuple(r["knock_key"] for r in pair["replies"])
    assert all(i.first_shown == DAY for i in items)
    positions = {v.answer: v.position for v in votes}
    assert positions == {"both_bad": 2, intent["generator_intent"]: 0}


def test_main_saves_trust_and_consensus_and_writes_the_export(db_url, tmp_path) -> None:
    with psycopg.connect(db_url) as conn:
        target = _crowd(conn, _seed(conn))

    assert (
        main(["--database-url", db_url, "--out-dir", str(tmp_path), "--today", "2026-10-27"]) == 0
    )

    export = json.loads((tmp_path / "consensus-2026-10-27.json").read_text())
    assert export["format"] == "knock-game-consensus"
    assert [(i["knock_key"], i["status"], i["label"]) for i in export["items"]] == [
        (target["knock_key"], "settled", target["generator_intent"])
    ]
    with psycopg.connect(db_url) as conn:
        trusts = [row[0] for row in conn.execute("SELECT trust FROM players")]
        statuses = dict(
            conn.execute("SELECT status, count(*) FROM consensus GROUP BY 1").fetchall()
        )
    assert all(t > 0.2 for t in trusts)
    assert statuses["settled"] == 1


def test_running_twice_gives_the_same_rows(db_url, tmp_path) -> None:
    with psycopg.connect(db_url) as conn:
        _crowd(conn, _seed(conn))
    snapshot = "SELECT item_id, label, share, total_weight, status FROM consensus ORDER BY item_id"
    args = ["--database-url", db_url, "--out-dir", str(tmp_path), "--today", "2026-10-27"]

    main(args)
    with psycopg.connect(db_url) as conn:
        first = conn.execute(snapshot).fetchall()
    main(args)
    with psycopg.connect(db_url) as conn:
        assert conn.execute(snapshot).fetchall() == first


def test_today_comes_from_the_database_by_default(db_url) -> None:
    with psycopg.connect(db_url) as conn:
        assert isinstance(db.today(conn), date)


def test_a_missing_database_url_is_an_error(monkeypatch) -> None:
    monkeypatch.delenv("DATABASE_URL", raising=False)
    with pytest.raises(SystemExit):
        main([])
