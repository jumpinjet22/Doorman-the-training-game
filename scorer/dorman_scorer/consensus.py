"""Turning weighted votes into per-item verdicts."""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass, field
from datetime import date

from dorman_scorer.config import Config
from dorman_scorer.model import Item, Vote

BOTH_BAD = "both_bad"


@dataclass
class Verdict:
    item_id: str
    item_key: str
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


def verdict(
    item: Item,
    votes: list[Vote],
    trust: dict[str, float],
    today: date,
    config: Config | None = None,
    posterior: dict[str, float] | None = None,
) -> Verdict:
    """`posterior` is Dawid-Skene's answer -> probability for this item.

    An item settles once it has `min_weight` of trust behind it and either the
    weighted share is overwhelming or
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
        status = "settled"
    elif item.first_shown is not None and today - item.first_shown >= config.dispute_after:
        status = "disputed"
    split = dict(sorted(weight_by_label.items()))
    return Verdict(item.id, item.item_key, item.mode, status, label, share, total, split, p_label)


def gold_verdicts(
    items: list[Item],
    votes: list[Vote],
    trust: dict[str, float],
    today: date,
    config: Config | None = None,
    posteriors: dict[str, dict[str, float]] | None = None,
) -> list[Verdict]:
    """Gold items' rows, for the reveal page, share cards and ranks only:
    decided by `verdict` from the crowd's votes, exactly as any other item
    is, and never from the known answer. A row that said "settled on the
    right answer" whatever the split would point at gold, and would hand the
    answer to anyone who saw it before the card came round again. Not
    counted in the run log; datasets take its answer from the item, not from here.

    `posteriors` should come from a Dawid-Skene pass that wasn't told the
    gold answers, so the posterior route to settling works the same way here.
    """
    by_item: dict[str, list[Vote]] = defaultdict(list)
    for vote in votes:
        by_item[vote.item_id].append(vote)
    posteriors = posteriors or {}
    return [
        verdict(i, by_item[i.id], trust, today, config, posteriors.get(i.id))
        for i in items
        if i.gold_answer is not None
    ]
