from __future__ import annotations

from dataclasses import dataclass, field
from datetime import date, datetime, timedelta


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
    # False for Overtime answers, which belong to no day. Only daily answers
    # earn trust on gold: Overtime serves past gold with its live split.
    daily: bool = True
    # When the player's account was created (None if unknown, which keeps
    # it out of cohort detection).
    joined: datetime | None = None


@dataclass(frozen=True)
class Item:
    id: str
    item_key: str
    mode: str  # "intent" | "pair"
    gold_answer: str | None
    intended_intent: str
    # Pair items only: the two reply item_keys, in display order.
    reply_keys: tuple[str, ...] = ()
    first_shown: date | None = None


@dataclass
class PlayerScore:
    trust: float
    flags: dict[str, float] = field(default_factory=dict)
