"""Brigade detection: a pile of brand-new accounts all pushing one answer on
the same item, against what established players say.

New accounts can't be told apart from honest newcomers one at a time, and
each one earns a little gold trust by answering gold right. Together they
can outweigh a handful of regulars. So when new accounts gang up on an item,
every new-account vote on that item counts for nothing in this scoring pass:
in Dawid-Skene and in the verdict. Their votes elsewhere, and their trust,
are untouched.
"""

from __future__ import annotations

from collections import Counter, defaultdict

from dorman_scorer.config import Config
from dorman_scorer.model import Item, Vote


def is_new(vote: Vote, config: Config) -> bool:
    return vote.account_age is not None and vote.account_age < config.new_account_age


def brigaded(
    items: dict[str, Item], votes: list[Vote], trust: dict[str, float], config: Config
) -> dict[str, set[str]]:
    """Item id -> players whose votes on it are zeroed. `trust` weighs the
    established players' votes (gold trust: it doesn't depend on any of the
    votes being judged here).
    """
    by_item: dict[str, list[Vote]] = defaultdict(list)
    for vote in votes:
        if items[vote.item_id].gold_answer is None:
            by_item[vote.item_id].append(vote)

    zeroed: dict[str, set[str]] = {}
    for item_id, item_votes in by_item.items():
        new = [v for v in item_votes if is_new(v, config)]
        pushed = {
            answer
            for answer, n in Counter(v.answer for v in new).items()
            if n >= config.brigade_min_votes
        }
        if not pushed:
            continue
        established: dict[str, float] = defaultdict(float)
        for vote in item_votes:
            if not is_new(vote, config):
                established[vote.answer] += trust.get(vote.player_id, 0.0)
        # A burst agreeing with a clear established leader is just a busy day.
        # With no established votes at all there is nothing to check it
        # against, so it's held back until regulars weigh in.
        ranked = sorted(established.values(), reverse=True)
        leader = max(established, key=established.__getitem__) if ranked else None
        clear = leader is not None and ranked[0] > 0 and (len(ranked) == 1 or ranked[0] > ranked[1])
        if clear and pushed == {leader}:
            continue
        zeroed[item_id] = {v.player_id for v in new}
    return zeroed
