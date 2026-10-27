"""Scorer thresholds, so soft launch can run with lower ones without a code
change. Every value has a CLI flag and an environment variable; see
`__main__`.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, fields, replace
from datetime import timedelta


@dataclass(frozen=True)
class Config:
    # Total trust behind an item before it can settle at all.
    min_weight: float = 5.0
    # Settle when the leading answer has this share of the weight...
    min_share: float = 0.8
    # ...or when Dawid-Skene puts at least this probability on it and it
    # still has a clear majority of the weight.
    min_posterior: float = 0.95
    min_posterior_share: float = 0.6
    # Unsettled this many days after first being shown: disputed.
    dispute_after_days: int = 3
    # Brigade detection: accounts younger than this when they voted are new...
    new_account_hours: float = 48.0
    # ...and this many new-account votes for one answer on one item, against
    # what established players say, is a brigade.
    brigade_min_votes: int = 8

    @property
    def dispute_after(self) -> timedelta:
        return timedelta(days=self.dispute_after_days)

    @property
    def new_account_age(self) -> timedelta:
        return timedelta(hours=self.new_account_hours)

    def as_dict(self) -> dict[str, float | int]:
        return asdict(self)


# Friends-only soft launch (plan M7): fewer players, so a lower bar to settle.
# Brigade detection is off: every account is new on launch day, and since an
# account's age is fixed when it votes, those votes would be zeroed for good.
# It's invite-only, so there's nobody to brigade. Dawid-Skene and the judge
# guard still apply.
SOFT_LAUNCH = Config(min_weight=2.0, min_share=0.7, new_account_hours=0.0)
PROFILES = {"default": Config(), "soft-launch": SOFT_LAUNCH}

# Field name -> environment variable that overrides it.
ENV = {f.name: "DORMAN_" + f.name.upper() for f in fields(Config)}


def build(profile: str = "default", **overrides: float | int | None) -> Config:
    """The named profile with any non-None overrides applied."""
    return replace(PROFILES[profile], **{k: v for k, v in overrides.items() if v is not None})
