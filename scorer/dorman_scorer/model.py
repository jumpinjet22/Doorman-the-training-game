from __future__ import annotations

from dataclasses import dataclass, field
from datetime import date, timedelta


@dataclass(frozen=True)
class Vote:
    player_id: str
    item_id: str
    answer: str
    ms_to_answer: int
    # Where the chosen answer sat in the list the player saw (None if unknown).
    position: int | None = None
    # How old the player's account was when they cast this vote (None if
    # unknown, which counts as established).
    account_age: timedelta | None = None


@dataclass(frozen=True)
class Item:
    id: str
    knock_key: str
    mode: str  # "intent" | "pair"
    gold_answer: str | None
    generator_intent: str
    # KNOCK's judge average for "is the generator's category right" (0-10).
    judge_category_correct: float | None = None
    # Pair items only: the two reply knock_keys, in display order.
    reply_keys: tuple[str, ...] = ()
    first_shown: date | None = None


@dataclass
class PlayerScore:
    trust: float
    flags: dict[str, float] = field(default_factory=dict)
