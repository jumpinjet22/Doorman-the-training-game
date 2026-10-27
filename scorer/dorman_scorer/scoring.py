"""One nightly pass: trust for every player, a verdict for every item."""

from __future__ import annotations

from collections import defaultdict
from datetime import date

from dorman_scorer.consensus import BOTH_BAD, Verdict, verdict
from dorman_scorer.dawid_skene import dawid_skene
from dorman_scorer.flags import behavior_flags
from dorman_scorer.model import Item, PlayerScore, Vote
from dorman_scorer.trust import gold_trust

# Below this many non-gold votes, Dawid-Skene knows too little about a player
# to move their trust either way.
MIN_VOTES_FOR_DS = 5


def _pair_slot(item: Item, answer: str) -> str:
    """Pair answers as positions, so every pair item shares one label space."""
    if answer == BOTH_BAD:
        return BOTH_BAD
    return "a" if item.reply_keys and answer == item.reply_keys[0] else "b"


def score(
    items: list[Item], votes: list[Vote], today: date
) -> tuple[dict[str, PlayerScore], list[Verdict]]:
    by_id = {item.id: item for item in items}
    votes = [v for v in votes if v.item_id in by_id]
    by_player: dict[str, list[Vote]] = defaultdict(list)
    for vote in votes:
        by_player[vote.player_id].append(vote)

    gold_trusts: dict[str, float] = {}
    for player, mine in by_player.items():
        gold_votes = [v for v in mine if by_id[v.item_id].gold_answer is not None]
        right = sum(1 for v in gold_votes if v.answer == by_id[v.item_id].gold_answer)
        gold_trusts[player] = gold_trust(right, len(gold_votes))

    # Dawid-Skene per mode, since intent and pair items answer different questions.
    ds_accuracy: dict[str, list[float]] = defaultdict(list)
    for mode in ("intent", "pair"):
        mode_votes = [v for v in votes if by_id[v.item_id].mode == mode]
        if not mode_votes:
            continue
        if mode == "intent":
            triples = [(v.player_id, v.item_id, v.answer) for v in mode_votes]
            labels = sorted(
                {v.answer for v in mode_votes}
                | {i.gold_answer for i in items if i.mode == mode and i.gold_answer}
            )
            gold = {i.id: i.gold_answer for i in items if i.mode == mode and i.gold_answer}
        else:
            triples = [
                (v.player_id, v.item_id, _pair_slot(by_id[v.item_id], v.answer)) for v in mode_votes
            ]
            labels = ["a", "b", BOTH_BAD]
            gold = {
                i.id: _pair_slot(i, i.gold_answer)
                for i in items
                if i.mode == mode and i.gold_answer
            }
        result = dawid_skene(triples, labels, init_weights=gold_trusts, gold=gold)
        chance = 1.0 / len(labels)
        for player, accuracy in result.accuracy.items():
            ds_accuracy[player].append(max(0.0, (accuracy - chance) / (1.0 - chance)))

    players: dict[str, PlayerScore] = {}
    for player, mine in by_player.items():
        trust = gold_trusts[player]
        non_gold = sum(1 for v in mine if by_id[v.item_id].gold_answer is None)
        if non_gold >= MIN_VOTES_FOR_DS and ds_accuracy[player]:
            trust *= min(1.0, sum(ds_accuracy[player]) / len(ds_accuracy[player]) + 0.2)
        flags = behavior_flags(mine)
        for multiplier in flags.values():
            trust *= multiplier
        players[player] = PlayerScore(trust=trust, flags=flags)

    trust_by_player = {p: s.trust for p, s in players.items()}
    item_votes: dict[str, list[Vote]] = defaultdict(list)
    for vote in votes:
        item_votes[vote.item_id].append(vote)
    verdicts = [
        verdict(item, item_votes[item.id], trust_by_player, today)
        for item in items
        if item.gold_answer is None
    ]
    return players, verdicts
