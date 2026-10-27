"""Turning weighted votes into per-item verdicts, and the export KNOCK reads."""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass, field
from datetime import date

from dorman_scorer.config import Config
from dorman_scorer.model import Item, Vote

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
    # Answer -> trust behind it, for the reveal page. Zeroed (brigade) votes
    # are left out.
    split: dict[str, float] = field(default_factory=dict)
    # Dawid-Skene's probability for `label`, when it had an opinion.
    posterior: float | None = None
    # Votes on this item zeroed as a brigade.
    zeroed_votes: int = 0


def _judge_disagrees(item: Item, label: str) -> bool:
    """The brigade guard: a crowd answer that contradicts a confident judge."""
    score = item.judge_category_correct
    if item.mode != "intent" or score is None:
        return False
    if score >= JUDGE_SURE_RIGHT and label != item.generator_intent:
        return True
    return score <= JUDGE_SURE_WRONG and label == item.generator_intent


def verdict(
    item: Item,
    votes: list[Vote],
    trust: dict[str, float],
    today: date,
    config: Config | None = None,
    posterior: dict[str, float] | None = None,
) -> Verdict:
    """`posterior` is Dawid-Skene's answer -> probability for this item.

    An item settles once it has `min_weight` of trust behind it, the judge
    doesn't contradict it, and either the weighted share is overwhelming or
    Dawid-Skene is confident and the weight still leans clearly the same way.
    The second path is what lets a noisy but honest crowd settle: 75%-right
    players never reach an 80% share, but their agreement is still strong
    evidence.
    """
    config = config or Config()
    weight_by_label: dict[str, float] = defaultdict(float)
    for vote in votes:
        weight_by_label[vote.answer] += trust.get(vote.player_id, 0.0)
    total = sum(weight_by_label.values())
    label, top = max(weight_by_label.items(), key=lambda kv: (kv[1], kv[0]), default=(None, 0.0))
    share = top / total if total else 0.0
    p_label = posterior.get(label, 0.0) if posterior is not None and label is not None else None

    confident = share >= config.min_share or (
        p_label is not None
        and p_label >= config.min_posterior
        and share >= config.min_posterior_share
    )
    status = "open"
    if label is not None and confident and total >= config.min_weight:
        status = "disputed" if _judge_disagrees(item, label) else "settled"
    elif item.first_shown is not None and today - item.first_shown >= config.dispute_after:
        status = "disputed"
    split = dict(sorted(weight_by_label.items()))
    return Verdict(item.id, item.knock_key, item.mode, status, label, share, total, split, p_label)


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
