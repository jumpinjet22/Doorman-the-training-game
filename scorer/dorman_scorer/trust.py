"""Gold-answer trust: a beta posterior over right/wrong gold answers."""

from __future__ import annotations

# Beta(1, 9): a brand-new player starts at a mean of 0.1, about a tenth of a
# veteran's vote, and has to earn the rest on gold items.
PRIOR_RIGHT = 1.0
PRIOR_WRONG = 9.0


def gold_trust(right: int, total: int) -> float:
    return (PRIOR_RIGHT + right) / (PRIOR_RIGHT + PRIOR_WRONG + total)
