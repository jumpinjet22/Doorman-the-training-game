"""dorman_scorer.db and the nightly entry point, against real Postgres."""

from __future__ import annotations

import json
import uuid
from datetime import date, timedelta
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
    """Loads the example batch and publishes it for DAY. Returns item_key -> id."""
    ids: dict[str, str] = {}
    for item in BATCH["items"]:
        payload = dict(item)
        if item["mode"] == "intent":
            others = [i for i in BATCH["intents"] if i != item["intended_intent"]]
            payload["options"] = [item["intended_intent"], *others[:3]]
        item_id = str(uuid.uuid4())
        ids[item["item_key"]] = item_id
        conn.execute(
            "INSERT INTO items (id, item_key, mode, payload, gold_answer, batch)"
            " VALUES (%s, %s, %s, %s, %s, %s)",
            (
                item_id,
                item["item_key"],
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


def _player(
    conn: psycopg.Connection,
    votes: list[tuple[str, str]],
    ms: int = 4000,
    age: timedelta = timedelta(days=30),
) -> str:
    """A player whose account was `age` old when they voted (just now)."""
    player = str(uuid.uuid4())
    conn.execute("INSERT INTO players (id, created_at) VALUES (%s, now() - %s)", (player, age))
    for item_id, answer in votes:
        conn.execute(
            "INSERT INTO votes (player_id, item_id, day, answer, ms_to_answer)"
            " VALUES (%s, %s, %s, %s, %s)",
            (player, item_id, DAY, answer, ms),
        )
    return player


def _crowd(conn, ids, n=20, answer=None, age=timedelta(days=30)):
    """n players who get every gold item right and agree on one open item."""
    gold = [(ids[i["item_key"]], i["gold_answer"]) for i in BATCH["items"] if i["gold_answer"]]
    target = next(i for i in BATCH["items"] if i["mode"] == "intent" and not i["gold_answer"])
    for _ in range(n):
        vote = (ids[target["item_key"]], answer or target["intended_intent"])
        _player(conn, [*gold, vote], age=age)
    return target


def _args(db_url: str, *extra: str) -> list[str]:
    return ["--database-url", db_url, "--today", DAY.isoformat(), *extra]


SNAPSHOT = {
    "players": "SELECT id, trust, flags FROM players ORDER BY id",
    "consensus": "SELECT item_id, label, share, total_weight, status, split"
    " FROM consensus ORDER BY item_id",
    "runs": "SELECT day, players, items, settled, disputed, brigaded, config"
    " FROM scoring_runs ORDER BY day",
}


def _snapshot(db_url: str) -> dict[str, list[tuple]]:
    with psycopg.connect(db_url) as conn:
        return {name: conn.execute(sql).fetchall() for name, sql in SNAPSHOT.items()}


def test_load_reads_items_votes_and_positions(db_url: str) -> None:
    with psycopg.connect(db_url) as conn:
        ids = _seed(conn)
        pair = next(i for i in BATCH["items"] if i["mode"] == "pair")
        intent = next(i for i in BATCH["items"] if i["mode"] == "intent")
        _player(
            conn,
            [
                (ids[pair["item_key"]], "both_bad"),
                (ids[intent["item_key"]], intent["intended_intent"]),
            ],
        )
        items, votes = db.load(conn)

    assert len(items) == len(BATCH["items"])
    loaded_pair = next(i for i in items if i.mode == "pair")
    assert loaded_pair.reply_keys == tuple(r["item_key"] for r in pair["replies"])
    assert all(i.first_shown == DAY for i in items)
    positions = {v.answer: v.position for v in votes}
    assert positions == {"both_bad": 2, intent["intended_intent"]: 0}
    assert all(timedelta(days=29) < v.account_age < timedelta(days=31) for v in votes)
    assert all(v.daily for v in votes)


def test_overtime_votes_load_as_not_daily(db_url: str) -> None:
    with psycopg.connect(db_url) as conn:
        ids = _seed(conn)
        intent = next(i for i in BATCH["items"] if i["mode"] == "intent")
        player = _player(conn, [(ids[intent["item_key"]], intent["intended_intent"])])
        conn.execute("UPDATE votes SET day = NULL WHERE player_id = %s", (player,))
        _, votes = db.load(conn)
    assert [v.daily for v in votes] == [False]


def test_main_saves_trust_and_consensus_and_writes_no_files(db_url, tmp_path, monkeypatch) -> None:
    with psycopg.connect(db_url) as conn:
        target = _crowd(conn, _seed(conn))
    monkeypatch.chdir(tmp_path)

    assert main(["--database-url", db_url, "--today", "2026-10-27"]) == 0

    assert list(tmp_path.iterdir()) == [], "results live in the database, not in files"
    with psycopg.connect(db_url) as conn:
        settled_keys = conn.execute(
            "SELECT i.item_key, c.status, c.label FROM consensus c JOIN items i ON i.id = c.item_id"
            " WHERE c.status = 'settled' AND i.gold_answer IS NULL"
        ).fetchall()
    assert settled_keys == [(target["item_key"], "settled", target["intended_intent"])]
    with psycopg.connect(db_url) as conn:
        trusts = [row[0] for row in conn.execute("SELECT trust FROM players")]
        statuses = dict(
            conn.execute("SELECT status, count(*) FROM consensus GROUP BY 1").fetchall()
        )
    assert all(t > 0.2 for t in trusts)
    gold = [i for i in BATCH["items"] if i["gold_answer"]]
    assert statuses["settled"] == 1 + len(gold), "gold gets a settled row too"


def test_running_twice_gives_the_same_rows(db_url, tmp_path) -> None:
    with psycopg.connect(db_url) as conn:
        _crowd(conn, _seed(conn))

    main(_args(db_url))
    first = _snapshot(db_url)
    main(_args(db_url))

    assert _snapshot(db_url) == first
    assert len(first["runs"]) == 1


def test_a_run_is_logged_and_a_rerun_replaces_it(db_url, tmp_path) -> None:
    with psycopg.connect(db_url) as conn:
        ids = _seed(conn)
        _crowd(conn, ids)
    main(_args(db_url))
    with psycopg.connect(db_url) as conn:
        _crowd(conn, ids, n=5)
    main(_args(db_url))

    with psycopg.connect(db_url) as conn:
        rows = conn.execute(
            "SELECT day, started_at <= finished_at, players, items, settled, disputed,"
            " brigaded, config FROM scoring_runs"
        ).fetchall()
    [(day, ordered, players, items, settled, disputed, brigaded, config)] = rows
    assert (day, ordered, players, items, settled, disputed, brigaded) == (
        DAY,
        True,
        25,
        3,
        1,
        0,
        0,
    )
    assert config["min_weight"] == 5.0 and config["brigade_min_votes"] == 8


def test_the_weighted_split_is_stored_for_the_reveal_page(db_url, tmp_path) -> None:
    with psycopg.connect(db_url) as conn:
        ids = _seed(conn)
        target = _crowd(conn, ids, n=18)
        _crowd(conn, ids, n=2, answer="soliciting")
    main(_args(db_url))

    with psycopg.connect(db_url) as conn:
        [(split, total)] = conn.execute(
            "SELECT split, total_weight FROM consensus WHERE item_id = %s",
            (ids[target["item_key"]],),
        ).fetchall()
    assert set(split) == {target["intended_intent"], "soliciting"}
    assert split[target["intended_intent"]] > 8 * split["soliciting"]
    assert sum(split.values()) == pytest.approx(total, rel=1e-4)


def test_a_crash_mid_run_leaves_no_partial_writes(db_url, monkeypatch) -> None:
    with psycopg.connect(db_url) as conn:
        ids = _seed(conn)
        _crowd(conn, ids)
    main(_args(db_url))
    before = _snapshot(db_url)
    with psycopg.connect(db_url) as conn:
        _crowd(conn, ids, n=10, answer="soliciting")

    def crash(*_args, **_kwargs):
        raise RuntimeError("power cut")

    # After every player and verdict has been written, before the run is logged.
    monkeypatch.setattr(db, "record_run", crash)
    with pytest.raises(RuntimeError, match="power cut"):
        main(_args(db_url))

    after = _snapshot(db_url)
    assert after["consensus"] == before["consensus"]
    assert after["runs"] == before["runs"]
    old_players = {row[0] for row in before["players"]}
    assert [r for r in after["players"] if r[0] in old_players] == before["players"]
    assert all(r[1] == pytest.approx(0.1) for r in after["players"] if r[0] not in old_players)


def test_a_burst_of_new_accounts_is_held_back(db_url, tmp_path) -> None:
    with psycopg.connect(db_url) as conn:
        ids = _seed(conn)
        target = _crowd(conn, ids, n=3)
        _crowd(conn, ids, n=30, answer="soliciting", age=timedelta(hours=2))
    main(_args(db_url))

    with psycopg.connect(db_url) as conn:
        [(label, status, split)] = conn.execute(
            "SELECT label, status, split FROM consensus WHERE item_id = %s",
            (ids[target["item_key"]],),
        ).fetchall()
        (brigaded,) = conn.execute("SELECT brigaded FROM scoring_runs").fetchone()
    assert (label, status, list(split)) == (target["intended_intent"], "open", [label])
    assert brigaded == 1


def test_soft_launch_thresholds_come_from_the_profile_env_and_flags(db_url, tmp_path) -> None:
    with psycopg.connect(db_url) as conn:
        _crowd(conn, _seed(conn), n=8)  # 8 x 4/13 = 2.46 trust: short of the default 5

    def status(*extra: str, env: dict[str, str] | None = None) -> str:
        main(_args(db_url, *extra), environ={**(env or {}), "DATABASE_URL": ""})
        with psycopg.connect(db_url) as conn:
            (value,) = conn.execute(
                "SELECT status FROM consensus WHERE label IS NOT NULL"
            ).fetchone()
            return value

    assert status() == "open"
    assert status("--profile", "soft-launch") == "settled"
    assert status(env={"DORMAN_SCORER_PROFILE": "soft-launch"}) == "settled"
    assert status(env={"DORMAN_MIN_WEIGHT": "2.4"}) == "settled"
    assert status("--min-weight", "4.5", env={"DORMAN_MIN_WEIGHT": "2.4"}) == "open"
    with psycopg.connect(db_url) as conn:
        (config,) = conn.execute("SELECT config FROM scoring_runs").fetchone()
    assert config["min_weight"] == 4.5


def test_today_comes_from_the_database_by_default(db_url) -> None:
    with psycopg.connect(db_url) as conn:
        assert isinstance(db.today(conn), date)


def test_a_missing_database_url_is_an_error() -> None:
    with pytest.raises(SystemExit):
        main([], environ={})


@pytest.mark.parametrize(
    "env",
    [{"DORMAN_SCORER_PROFILE": "bogus"}, {"DORMAN_MIN_SHARE": "lots"}],
)
def test_bad_configuration_is_an_error(env) -> None:
    with pytest.raises(SystemExit):
        main(["--database-url", "postgres://unused"], environ=env)


def test_gold_gets_a_settled_row_but_stays_out_of_the_log(db_url) -> None:
    with psycopg.connect(db_url) as conn:
        ids = _seed(conn)
        _crowd(conn, ids)
    main(_args(db_url))

    gold = {ids[i["item_key"]]: i["gold_answer"] for i in BATCH["items"] if i["gold_answer"]}
    with psycopg.connect(db_url) as conn:
        rows = conn.execute(
            "SELECT item_id::text, status, label, split FROM consensus WHERE item_id = ANY (%s)",
            (list(gold),),
        ).fetchall()
        [(items, settled)] = conn.execute("SELECT items, settled FROM scoring_runs").fetchall()
    assert {item_id: (status, label) for item_id, status, label, _ in rows} == {
        item_id: ("settled", answer) for item_id, answer in gold.items()
    }
    assert all(set(split) == {gold[item_id]} for item_id, _, _, split in rows)
    assert (items, settled) == (3, 1), "the run log counts only non-gold items"
