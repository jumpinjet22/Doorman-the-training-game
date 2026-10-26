"""Turning weighted votes into per-item verdicts, and the export KNOCK reads."""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass
from datetime import date, timedelta

from dorman_scorer.model import Item, Vote

MIN_SHARE = 0.8
MIN_WEIGHT = 5.0
DISPUTE_AFTER = timedelta(days=3)
BOTH_BAD = "both_bad"
# KNOCK judge category_correct averages (0-10) treated as a confident verdict.
JUDGE_SURE_RIGHT = 7.0
JUDGE_SURE_WRONG = 3.0


@dataclass
class Verdict:
    item_id: str
    knock_key: str
    mode: str
    status: str  # "open" | "settled" | "disputed"
    label: str | None
    share: float
    total_weight: float


def _judge_disagrees(item: Item, label: str) -> bool:
    """The brigade guard: a crowd answer that contradicts a confident judge."""
    score = item.judge_category_correct
    if item.mode != "intent" or score is None:
        return False
    if score >= JUDGE_SURE_RIGHT and label != item.generator_intent:
        return True
    return score <= JUDGE_SURE_WRONG and label == item.generator_intent


def verdict(item: Item, votes: list[Vote], trust: dict[str, float], today: date) -> Verdict:
    weight_by_label: dict[str, float] = defaultdict(float)
    for vote in votes:
        weight_by_label[vote.answer] += trust.get(vote.player_id, 0.0)
    total = sum(weight_by_label.values())
    label, top = max(weight_by_label.items(), key=lambda kv: (kv[1], kv[0]), default=(None, 0.0))
    share = top / total if total else 0.0

    status = "open"
    if label is not None and share >= MIN_SHARE and total >= MIN_WEIGHT:
        status = "disputed" if _judge_disagrees(item, label) else "settled"
    elif item.first_shown is not None and today - item.first_shown >= DISPUTE_AFTER:
        status = "disputed"
    return Verdict(item.id, item.knock_key, item.mode, status, label, share, total)


def consensus_file(day: date, verdicts: list[Verdict]) -> dict[str, object]:
    """`knock-game-consensus` v1, as KNOCK's import_game_consensus.py reads it.
    Gold items are left out: KNOCK already knows those answers.
    """
    return {
        "format": "knock-game-consensus",
        "version": 1,
        "day": day.isoformat(),
        "items": [
            {
                "knock_key": v.knock_key,
                "mode": v.mode,
                "status": v.status,
                "label": v.label,
                "share": round(v.share, 4),
                "total_weight": round(v.total_weight, 3),
            }
            for v in verdicts
        ],
    }
