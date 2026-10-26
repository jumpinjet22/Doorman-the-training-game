"""Behavior flags. Each one multiplies a player's trust down rather than
banning them; players are never told.
"""

from __future__ import annotations

import statistics
from collections import Counter

from dorman_scorer.model import Vote

FAST_MEDIAN_MS = 1200
POSITION_BIAS_SHARE = 0.7
MIN_VOTES_FOR_FLAGS = 6
PENALTY = 0.3


def behavior_flags(votes: list[Vote]) -> dict[str, float]:
    """Flag name -> trust multiplier for one player's votes."""
    if len(votes) < MIN_VOTES_FOR_FLAGS:
        return {}
    flags: dict[str, float] = {}
    if statistics.median(v.ms_to_answer for v in votes) < FAST_MEDIAN_MS:
        flags["too_fast"] = PENALTY
    positions = [v.position for v in votes if v.position is not None]
    if len(positions) >= MIN_VOTES_FOR_FLAGS:
        _, top = Counter(positions).most_common(1)[0]
        if top / len(positions) > POSITION_BIAS_SHARE:
            flags["same_position"] = PENALTY
        flips = all(a != b for a, b in zip(positions, positions[1:], strict=False))
        if len(positions) >= 8 and flips and len(set(positions)) == 2:
            flags["alternating"] = PENALTY
    return flags
