"""Cohort detection: accounts that signed up close together and then keep
voting identically, item after item.

The brigade check (`brigade`) only sees accounts that are new when they vote,
and only when eight or more of them pile onto one answer on one item. A farm
that signs up early and waits, or spreads itself so no item gets eight of
its votes, gets past it. Its accounts still give themselves away together:
they joined within days of each other and they answer as one. Honest
players who joined the same week agree often, but not on nearly everything,
and where they do agree it's with everyone else.

So accounts are linked when they joined within `cohort_window_hours` of each
other and gave the same answer on at least `cohort_agreement` of at least
`cohort_min_shared` shared non-gold items; a connected group of at least
`cohort_min_size` is a cohort. On each item where a cohort's answer isn't
the clear choice of everyone outside it, the cohort's votes count for
nothing, exactly as a brigade's do. Gold is left out: answering it right is
what the farm wants to look like.
"""

from __future__ import annotations

from collections import Counter, defaultdict
from datetime import datetime

from dorman_scorer.brigade import is_new
from dorman_scorer.config import Config
from dorman_scorer.model import Item, Vote


def cohorts(items: dict[str, Item], votes: list[Vote], config: Config) -> list[set[str]]:
    """Groups of accounts that joined together and vote as one."""
    joined: dict[str, datetime] = {}
    answers: dict[str, dict[str, str]] = defaultdict(dict)
    for vote in votes:
        if vote.joined is not None:
            joined[vote.player_id] = vote.joined
        if items[vote.item_id].gold_answer is None:
            answers[vote.player_id][vote.item_id] = vote.answer

    parent = {player: player for player in joined}

    def root(player: str) -> str:
        while parent[player] != player:
            parent[player] = parent[parent[player]]
            player = parent[player]
        return player

    order = sorted(joined, key=lambda p: (joined[p], p))
    for i, a in enumerate(order):
        mine = answers.get(a, {})
        for b in order[i + 1 :]:
            if joined[b] - joined[a] > config.cohort_window:
                break
            theirs = answers.get(b, {})
            shared = mine.keys() & theirs.keys()
            if len(shared) < config.cohort_min_shared:
                continue
            same = sum(1 for item in shared if mine[item] == theirs[item])
            if same >= config.cohort_agreement * len(shared):
                parent[root(a)] = root(b)

    groups: dict[str, set[str]] = defaultdict(set)
    for player in joined:
        groups[root(player)].add(player)
    found = [g for g in groups.values() if len(g) >= config.cohort_min_size]
    return sorted(found, key=lambda g: sorted(g))


def cohort_zeroed(
    items: dict[str, Item], votes: list[Vote], trust: dict[str, float], config: Config
) -> dict[str, set[str]]:
    """Item id -> cohort members whose votes on it are zeroed. `trust` weighs
    everyone else's votes (gold trust, as in `brigade`)."""
    found = cohorts(items, votes, config)
    member = {player: n for n, group in enumerate(found) for player in group}
    by_item: dict[str, list[Vote]] = defaultdict(list)
    for vote in votes:
        if items[vote.item_id].gold_answer is None:
            by_item[vote.item_id].append(vote)

    zeroed: dict[str, set[str]] = defaultdict(set)
    for item_id, item_votes in by_item.items():
        outside: dict[str, float] = defaultdict(float)
        for vote in item_votes:
            if vote.player_id not in member and not is_new(vote, config):
                outside[vote.answer] += trust.get(vote.player_id, 0.0)
        ranked = sorted(outside.values(), reverse=True)
        leader = max(outside, key=outside.__getitem__) if ranked else None
        clear = leader is not None and ranked[0] > 0 and (len(ranked) == 1 or ranked[0] > ranked[1])
        for n in range(len(found)):
            theirs = [v for v in item_votes if member.get(v.player_id) == n]
            if not theirs:
                continue
            (pushed, _), *_ = Counter(v.answer for v in theirs).most_common()
            # A cohort that agrees with a clear outside leader is friends who
            # signed up together, or a farm doing no harm here.
            if clear and pushed == leader:
                continue
            zeroed[item_id].update(v.player_id for v in theirs)
    return dict(zeroed)
