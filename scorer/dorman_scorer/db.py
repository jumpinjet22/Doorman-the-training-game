"""Postgres reads and writes for the nightly run (schema: crates/db/migrations)."""

from __future__ import annotations

import json
from collections.abc import Iterable
from dataclasses import dataclass, field
from datetime import date

import psycopg

from dorman_scorer.consensus import Verdict
from dorman_scorer.model import Item, PlayerScore, Vote


def load(conn: psycopg.Connection) -> tuple[list[Item], list[Vote]]:
    items: list[Item] = []
    options: dict[str, list[str]] = {}
    rows = conn.execute(
        """SELECT i.id::text, i.knock_key, i.mode, i.gold_answer, i.payload,
                  (SELECT min(d.day) FROM daily_sets d WHERE i.id = ANY (d.item_ids))
           FROM items i"""
    ).fetchall()
    for item_id, knock_key, mode, gold, payload, first_shown in rows:
        judge = payload.get("judge") or {}
        replies = payload.get("replies") or []
        items.append(
            Item(
                id=item_id,
                knock_key=knock_key,
                mode=mode,
                gold_answer=gold,
                generator_intent=payload.get("generator_intent", ""),
                judge_category_correct=judge.get("category_correct"),
                reply_keys=tuple(r["knock_key"] for r in replies),
                first_shown=first_shown,
            )
        )
        choices = payload.get("options") or [r["knock_key"] for r in replies] + ["both_bad"]
        options[item_id] = choices

    votes = []
    for player_id, item_id, answer, ms, age, daily, joined in conn.execute(
        """SELECT v.player_id::text, v.item_id::text, v.answer, v.ms_to_answer,
                  v.created_at - p.created_at, v.day IS NOT NULL, p.created_at
           FROM votes v JOIN players p ON p.id = v.player_id"""
    ):
        choices = options.get(item_id, [])
        position = choices.index(answer) if answer in choices else None
        votes.append(Vote(player_id, item_id, answer, ms, position, age, daily, joined))
    return items, votes


def lock(conn: psycopg.Connection) -> None:
    """Holds off any other scorer run until this transaction ends."""
    conn.execute("SELECT pg_advisory_xact_lock(hashtext('dorman-scorer'))")


def save(
    conn: psycopg.Connection, players: dict[str, PlayerScore], verdicts: Iterable[Verdict]
) -> None:
    """Call inside the run's transaction: on its own this commits nothing."""
    for player_id, score in players.items():
        conn.execute(
            "UPDATE players SET trust = %s, flags = %s WHERE id = %s::uuid",
            (score.trust, json.dumps(score.flags), player_id),
        )
    for v in verdicts:
        conn.execute(
            """INSERT INTO consensus
                 (item_id, label, share, total_weight, status, split, updated_at)
               VALUES (%s::uuid, %s, %s, %s, %s, %s, now())
               ON CONFLICT (item_id) DO UPDATE SET label = excluded.label,
                 share = excluded.share, total_weight = excluded.total_weight,
                 status = excluded.status, split = excluded.split, updated_at = now()""",
            (v.item_id, v.label, v.share, v.total_weight, v.status, json.dumps(v.split)),
        )


@dataclass
class Run:
    day: date
    players: int
    items: int
    settled: int
    disputed: int
    brigaded: int
    export_path: str
    config: dict[str, float | int]
    # Settings that switched a defence off for this run, in words.
    warnings: list[str] = field(default_factory=list)


def record_run(conn: psycopg.Connection, run: Run) -> None:
    """Logs a finished run, replacing an earlier run for the same day.
    started_at is the transaction's start, finished_at the wall clock now.
    """
    conn.execute(
        """INSERT INTO scoring_runs (day, started_at, finished_at, players, items, settled,
                                     disputed, brigaded, export_path, config, warnings)
           VALUES (%s, now(), clock_timestamp(), %s, %s, %s, %s, %s, %s, %s, %s)
           ON CONFLICT (day) DO UPDATE SET started_at = excluded.started_at,
             finished_at = excluded.finished_at, players = excluded.players,
             items = excluded.items, settled = excluded.settled,
             disputed = excluded.disputed, brigaded = excluded.brigaded,
             export_path = excluded.export_path, config = excluded.config,
             warnings = excluded.warnings""",
        (
            run.day,
            run.players,
            run.items,
            run.settled,
            run.disputed,
            run.brigaded,
            run.export_path,
            json.dumps(run.config),
            json.dumps(run.warnings),
        ),
    )


def today(conn: psycopg.Connection) -> date:
    (value,) = conn.execute("SELECT (now() AT TIME ZONE 'UTC')::date").fetchone() or (None,)
    assert isinstance(value, date)
    return value
