"""Postgres reads and writes for the nightly run (schema: crates/db/migrations)."""

from __future__ import annotations

import json
from collections.abc import Iterable
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
    for player_id, item_id, answer, ms in conn.execute(
        "SELECT player_id::text, item_id::text, answer, ms_to_answer FROM votes"
    ):
        choices = options.get(item_id, [])
        position = choices.index(answer) if answer in choices else None
        votes.append(Vote(player_id, item_id, answer, ms, position))
    return items, votes


def save(
    conn: psycopg.Connection, players: dict[str, PlayerScore], verdicts: Iterable[Verdict]
) -> None:
    with conn.transaction():
        for player_id, score in players.items():
            conn.execute(
                "UPDATE players SET trust = %s, flags = %s WHERE id = %s::uuid",
                (score.trust, json.dumps(score.flags), player_id),
            )
        for v in verdicts:
            conn.execute(
                """INSERT INTO consensus (item_id, label, share, total_weight, status, updated_at)
                   VALUES (%s::uuid, %s, %s, %s, %s, now())
                   ON CONFLICT (item_id) DO UPDATE SET label = excluded.label,
                     share = excluded.share, total_weight = excluded.total_weight,
                     status = excluded.status, updated_at = now()""",
                (v.item_id, v.label, v.share, v.total_weight, v.status),
            )


def today(conn: psycopg.Connection) -> date:
    (value,) = conn.execute("SELECT (now() AT TIME ZONE 'UTC')::date").fetchone() or (None,)
    assert isinstance(value, date)
    return value
