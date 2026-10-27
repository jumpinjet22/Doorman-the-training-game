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
# Giving one answer to nearly everything earns gold trust whenever the gold
# happens to lean that way, without reading a single card. Honest players
# only get near this when nearly every item really is the same.
SAME_ANSWER_SHARE = 0.9
MIN_VOTES_FOR_SAME_ANSWER = 10
PENALTY = 0.3


def behavior_flags(votes: list[Vote]) -> dict[str, float]:
    """Flag name -> trust multiplier for one player's votes."""
    if len(votes) < MIN_VOTES_FOR_FLAGS:
        return {}
    flags: dict[str, float] = {}
    if statistics.median(v.ms_to_answer for v in votes) < FAST_MEDIAN_MS:
        flags["too_fast"] = PENALTY
    if len(votes) >= MIN_VOTES_FOR_SAME_ANSWER:
        _, top = Counter(v.answer for v in votes).most_common(1)[0]
        if top / len(votes) > SAME_ANSWER_SHARE:
            flags["same_answer"] = PENALTY
    positions = [v.position for v in votes if v.position is not None]
    if len(positions) >= MIN_VOTES_FOR_FLAGS:
        _, top = Counter(positions).most_common(1)[0]
        if top / len(positions) > POSITION_BIAS_SHARE:
            flags["same_position"] = PENALTY
        flips = all(a != b for a, b in zip(positions, positions[1:], strict=False))
        if len(positions) >= 8 and flips and len(set(positions)) == 2:
            flags["alternating"] = PENALTY
    return flags
