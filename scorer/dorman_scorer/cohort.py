"""Cohort detection: accounts that signed up close together and then keep
giving the same answers against everyone else, item after item.

The brigade check (`brigade`) only sees accounts that are new when they vote,
and only when eight or more of them pile onto one answer on one item. A farm
that signs up early and waits, or spreads itself so no item gets eight of
its votes, gets past it. Its accounts still give themselves away together:
they joined within days of each other, and they push the same wrong answers.

Agreeing on the right answer is no evidence of anything (good players who
joined on launch day agree on nearly every item), so only answers that go
against the crowd link two accounts: an answer both gave on an item where,
among everyone else who answered it, that answer isn't the most common.
Accounts are linked when they joined within `cohort_window_hours` of each
other, share at least `cohort_min_shared` such answers, and agree on at least
`cohort_agreement` of all the items they both answered. A connected group of
at least `cohort_min_size` is a cohort. On each item where a cohort's answer
isn't the clear choice of everyone outside it, the cohort's votes count for
nothing, exactly as a brigade's do. Gold is left out: answering it right is
what the farm wants to look like.

Only accounts with enough against-the-crowd answers are ever paired, through
an index of those answers, so the cost grows with how much contrarian voting
there is, not with how many people joined on the same day. The counts are
raw, not trust-weighted: a farm big enough to be the plurality on its items
looks like the crowd here, and is left to Dawid-Skene and the brigade check.
"""

from __future__ import annotations

from collections import Counter, defaultdict
from datetime import datetime
from itertools import combinations

from dorman_scorer.brigade import is_new
from dorman_scorer.config import Config
from dorman_scorer.model import Item, Vote


def _against(counts: Counter[str], answer: str) -> bool:
    """Whether `answer`, given by two of the voters counted in `counts`, isn't
    a most-common answer among the rest. With nobody else, it isn't against
    anyone."""
    rest = counts[answer] - 2
    others = [n for a, n in counts.items() if a != answer]
    top = max([rest, *others])
    return top > 0 and rest < top


def cohorts(items: dict[str, Item], votes: list[Vote], config: Config) -> list[set[str]]:
    """Groups of accounts that joined together and vote as one against the
    crowd."""
    joined: dict[str, datetime] = {}
    answers: dict[str, dict[str, str]] = defaultdict(dict)
    counts: dict[str, Counter[str]] = defaultdict(Counter)
    for vote in votes:
        if vote.joined is not None:
            joined[vote.player_id] = vote.joined
        if items[vote.item_id].gold_answer is None:
            answers[vote.player_id][vote.item_id] = vote.answer
            counts[vote.item_id][vote.answer] += 1

    # (item, answer) -> players who gave it, for answers that would go against
    # the crowd if another account gave them too.
    contrarian: dict[tuple[str, str], list[str]] = defaultdict(list)
    for player in joined:
        for item_id, answer in answers.get(player, {}).items():
            if counts[item_id][answer] >= 2 and _against(counts[item_id], answer):
                contrarian[(item_id, answer)].append(player)
    together: Counter[tuple[str, str]] = Counter()
    for players in contrarian.values():
        for a, b in combinations(sorted(players), 2):
            together[(a, b)] += 1

    parent = {player: player for player in joined}

    def root(player: str) -> str:
        while parent[player] != player:
            parent[player] = parent[parent[player]]
            player = parent[player]
        return player

    for (a, b), n in together.items():
        if n < config.cohort_min_shared or abs(joined[a] - joined[b]) > config.cohort_window:
            continue
        mine, theirs = answers[a], answers[b]
        shared = mine.keys() & theirs.keys()
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
