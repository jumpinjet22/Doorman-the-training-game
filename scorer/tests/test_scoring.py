import random
from datetime import date, timedelta

import numpy as np

from dorman_scorer import config
from dorman_scorer.brigade import brigaded
from dorman_scorer.config import Config
from dorman_scorer.consensus import gold_verdicts, verdict
from dorman_scorer.dawid_skene import dawid_skene
from dorman_scorer.flags import behavior_flags
from dorman_scorer.model import Item, Vote
from dorman_scorer.scoring import score
from dorman_scorer.trust import gold_trust

TODAY = date(2026, 10, 27)
LABELS = ["delivery", "soliciting", "visitation", "unknown"]


def _item(n: int, gold: str | None = None, **kw) -> Item:
    defaults = dict(
        id=f"item{n}",
        item_key=f"k{n}",
        mode="intent",
        gold_answer=gold,
        intended_intent="delivery",
        first_shown=TODAY,
    )
    defaults.update(kw)
    return Item(**defaults)


def _vote(player: str, item: Item, answer: str, ms: int = 4000, pos: int | None = None) -> Vote:
    return Vote(player, item.id, answer, ms, pos)


def test_new_players_start_at_a_tenth_and_earn_trust_on_gold() -> None:
    assert gold_trust(0, 0) == 0.1
    assert gold_trust(20, 20) > 0.6
    assert gold_trust(0, 20) < 0.04


def test_dawid_skene_recovers_truth_and_spots_a_bad_player() -> None:
    rng = np.random.default_rng(0)
    truth = {f"i{n}": LABELS[n % 3] for n in range(40)}
    votes = []
    for item, label in truth.items():
        for p in range(5):  # good players, 90% right
            answer = label if rng.random() < 0.9 else LABELS[int(rng.integers(0, 4))]
            votes.append((f"good{p}", item, answer))
        votes.append(("troll", item, "unknown"))

    result = dawid_skene(votes, LABELS)

    estimated = {item: LABELS[int(np.argmax(post))] for item, post in result.posteriors.items()}
    assert sum(estimated[i] == truth[i] for i in truth) >= 38
    assert result.accuracy["troll"] < 0.4 < min(result.accuracy[f"good{p}"] for p in range(5))


def test_gold_items_are_clamped() -> None:
    result = dawid_skene([("p", "g", "soliciting")], LABELS, gold={"g": "delivery"})
    assert LABELS[int(np.argmax(result.posteriors["g"]))] == "delivery"


def test_behavior_flags() -> None:
    item = _item(1)
    assert behavior_flags([_vote("p", item, "x", ms=500, pos=0)] * 8) == {
        "too_fast": 0.3,
        "same_position": 0.3,
    }
    alternating = [_vote("p", item, "x", pos=i % 2) for i in range(10)]
    assert "alternating" in behavior_flags(alternating)
    assert behavior_flags([_vote("p", item, "x", pos=i % 4) for i in range(8)]) == {}
    assert behavior_flags([_vote("p", item, "x", ms=100)] * 3) == {}  # too few to judge


def test_settles_on_share_and_weight_and_disputes_after_three_days() -> None:
    item = _item(1)
    trust = {f"p{n}": 1.0 for n in range(10)}
    votes = [_vote(f"p{n}", item, "delivery") for n in range(9)] + [_vote("p9", item, "soliciting")]
    v = verdict(item, votes, trust, TODAY)
    assert (v.status, v.label, round(v.share, 2)) == ("settled", "delivery", 0.9)
    assert v.total_weight == 10

    split = [_vote(f"p{n}", item, "delivery" if n < 6 else "soliciting") for n in range(10)]
    assert verdict(item, split, trust, TODAY).status == "open"
    old = _item(1, first_shown=TODAY - timedelta(days=3))
    assert verdict(old, split, trust, TODAY).status == "disputed"


def test_trolls_stop_mattering() -> None:
    # Regulars who've played 20 gold items; 10 isn't enough trust to settle alone.
    gold = [_item(n, gold="delivery") for n in range(20)]
    data = [_item(100 + n) for n in range(10)]
    votes = []
    for p in range(8):  # honest regulars: right on gold, agree on data
        votes += [_vote(f"honest{p}", g, "delivery") for g in gold]
        votes += [_vote(f"honest{p}", d, "visitation") for d in data]
    for t in range(20):  # a brigade of fresh accounts, wrong on gold, fast
        votes += [_vote(f"troll{t}", g, "soliciting", ms=600) for g in gold[:3]]
        votes += [_vote(f"troll{t}", d, "soliciting", ms=600) for d in data]

    players, verdicts = score(gold + data, votes, TODAY)

    assert max(players[f"troll{t}"].trust for t in range(20)) < 0.05
    assert min(players[f"honest{p}"].trust for p in range(8)) > 0.4
    assert {(v.status, v.label) for v in verdicts} == {("settled", "visitation")}
    assert all(v.item_id.startswith("item1") for v in verdicts)  # gold not in verdicts


def test_pair_answers_map_through_positions() -> None:
    pair = _item(1, mode="pair", reply_keys=("ra", "rb"), gold_answer="ra")
    open_pair = _item(2, mode="pair", reply_keys=("rc", "rd"))
    votes = [_vote(f"p{n}", pair, "ra") for n in range(3)]
    votes += [_vote(f"p{n}", open_pair, "rd") for n in range(3)]
    _, verdicts = score([pair, open_pair], votes, TODAY)
    [v] = verdicts
    assert v.label == "rd"


def test_same_answer_to_everything_is_flagged() -> None:
    items = [_item(n) for n in range(12)]
    assert behavior_flags([_vote("p", i, "delivery", pos=n % 4) for n, i in enumerate(items)]) == {
        "same_answer": 0.3
    }
    varied = [_vote("p", i, LABELS[n % 2], pos=n % 4) for n, i in enumerate(items)]
    assert behavior_flags(varied) == {}


def test_settles_on_a_confident_posterior_with_a_clear_majority() -> None:
    item = _item(1)
    trust = {f"p{n}": 1.0 for n in range(10)}
    votes = [_vote(f"p{n}", item, "delivery" if n < 7 else "soliciting") for n in range(10)]
    sure = {"delivery": 0.99, "soliciting": 0.01}

    v = verdict(item, votes, trust, TODAY, posterior=sure)
    assert (v.status, v.label, v.posterior) == ("settled", "delivery", 0.99)
    assert v.split == {"delivery": 7.0, "soliciting": 3.0}
    assert verdict(item, votes, trust, TODAY).status == "open"  # 70% share alone isn't enough
    unsure = {"delivery": 0.9, "soliciting": 0.1}
    assert verdict(item, votes, trust, TODAY, posterior=unsure).status == "open"
    # Dawid-Skene can't settle an item the weight is split on...
    close = [_vote(f"p{n}", item, "delivery" if n < 5 else "soliciting") for n in range(9)]
    assert verdict(item, close, trust, TODAY, posterior=sure).status == "open"
    # ...nor skip the weight floor.
    thin = {p: 0.4 for p in trust}
    assert verdict(item, votes, thin, TODAY, posterior=sure).status == "open"


def test_thresholds_come_from_config() -> None:
    item = _item(1)
    trust = {f"p{n}": 0.5 for n in range(6)}
    votes = [_vote(f"p{n}", item, "delivery" if n < 5 else "visitation") for n in range(6)]
    assert verdict(item, votes, trust, TODAY).status == "open"
    assert verdict(item, votes, trust, TODAY, config.SOFT_LAUNCH).status == "settled"
    stale = _item(1, first_shown=TODAY - timedelta(days=1))
    quick = config.build(dispute_after_days=1)
    assert verdict(stale, votes, trust, TODAY, quick).status == "disputed"
    assert config.build("soft-launch", min_weight=None, min_share=0.9).as_dict()["min_share"] == 0.9


NEW, OLD = timedelta(hours=3), timedelta(days=30)


def _brigade_votes(item: Item, established: list[str], burst: str, n: int = 8) -> list[Vote]:
    votes = [Vote(f"reg{k}", item.id, a, 4000, account_age=OLD) for k, a in enumerate(established)]
    return votes + [Vote(f"new{k}", item.id, burst, 4000, account_age=NEW) for k in range(n)]


def test_a_burst_of_new_accounts_against_the_regulars_is_zeroed() -> None:
    item, items = _item(1), {"item1": _item(1)}
    trust = {f"reg{k}": 0.6 for k in range(3)}
    votes = _brigade_votes(item, ["delivery"] * 3, "soliciting")
    assert brigaded(items, votes, trust, Config()) == {"item1": {f"new{k}" for k in range(8)}}
    # Every new-account vote on the item goes, not just the brigade's answer.
    votes.append(Vote("newbie", item.id, "delivery", 4000, account_age=NEW))
    assert "newbie" in brigaded(items, votes, trust, Config())["item1"]


def test_bursts_that_agree_or_are_small_are_left_alone() -> None:
    item, items = _item(1), {"item1": _item(1)}
    trust = {f"reg{k}": 0.6 for k in range(3)}
    agreeing = _brigade_votes(item, ["delivery"] * 3, "delivery", n=30)
    assert brigaded(items, agreeing, trust, Config()) == {}
    small = _brigade_votes(item, ["delivery"] * 3, "soliciting", n=7)
    assert brigaded(items, small, trust, Config()) == {}
    unknown_age = [Vote(f"x{k}", item.id, "soliciting", 4000) for k in range(20)]
    assert brigaded(items, unknown_age, trust, Config()) == {}
    gold = {"item1": _item(1, gold="delivery")}
    assert brigaded(gold, _brigade_votes(item, [], "soliciting"), trust, Config()) == {}


def test_a_burst_with_no_clear_established_leader_is_held_back() -> None:
    item, items = _item(1), {"item1": _item(1)}
    trust = {"reg0": 0.5, "reg1": 0.5}
    alone = _brigade_votes(item, [], "delivery")
    assert set(brigaded(items, alone, trust, Config())) == {"item1"}
    tied = _brigade_votes(item, ["delivery", "soliciting"], "delivery")
    assert set(brigaded(items, tied, trust, Config())) == {"item1"}


def test_score_zeroes_a_brigade_everywhere() -> None:
    gold = [_item(n, gold="delivery") for n in range(10)]
    target = _item(100)
    votes = []
    for p in range(6):
        votes += [Vote(f"reg{p}", g.id, "delivery", 4000, account_age=OLD) for g in gold]
        votes.append(Vote(f"reg{p}", target.id, "visitation", 4000, account_age=OLD))
    for b in range(40):
        votes += [Vote(f"new{b}", g.id, "delivery", 4000, account_age=NEW) for g in gold[:3]]
        votes.append(Vote(f"new{b}", target.id, "soliciting", 4000, account_age=NEW))

    _, [v] = score([*gold, target], votes, TODAY)

    assert (v.label, v.zeroed_votes, set(v.split)) == ("visitation", 40, {"visitation"})


def test_pair_posteriors_come_back_as_reply_keys() -> None:
    gold = [
        _item(n, mode="pair", reply_keys=(f"r{n}a", f"r{n}b"), gold_answer=f"r{n}a")
        for n in range(6)
    ]
    target = _item(100, mode="pair", reply_keys=("xa", "xb"))
    votes = []
    for p in range(12):
        votes += [_vote(f"p{p}", g, g.reply_keys[0]) for g in gold]
        votes.append(_vote(f"p{p}", target, "xb" if p < 8 else "both_bad"))
    _, [v] = score([*gold, target], votes, TODAY)
    assert v.label == "xb" and v.posterior is not None and v.posterior > 0.9


def test_gold_items_are_decided_by_the_crowd_like_any_other_item() -> None:
    gold, plain = _item(1, gold="delivery"), _item(2)
    votes = [
        _vote("a", gold, "delivery"),
        _vote("b", gold, "delivery"),
        _vote("c", gold, "soliciting"),
        _vote("a", plain, "delivery"),
    ]
    trust = {"a": 2.0, "b": 2.0, "c": 1.0}
    [row] = gold_verdicts([gold, plain], votes, trust, TODAY)
    same = verdict(gold, votes, trust, TODAY)
    assert (row.item_id, row.status, row.label) == ("item1", same.status, same.label)
    assert row.split == {"delivery": 4.0, "soliciting": 1.0}
    assert (row.share, row.total_weight) == (0.8, 5.0)

    # The crowd leaning the wrong way is what the row says, never the key.
    wrong = [_vote(p, gold, "soliciting") for p in "abc"] + [_vote("d", gold, "delivery")]
    [row] = gold_verdicts([gold], wrong, {"a": 2.0, "b": 2.0, "c": 2.0, "d": 1.0}, TODAY)
    assert (row.status, row.label) == ("settled", "soliciting")

    # A split crowd doesn't settle, gold or not: no "settled" next to a split
    # that disagrees with it.
    split = [_vote("a", gold, "delivery"), _vote("b", gold, "soliciting")]
    [row] = gold_verdicts([gold], split, {"a": 3.0, "b": 3.0}, TODAY)
    assert row.status == "open"

    # Nobody voted yet: open with nothing behind it, as any item would be.
    [empty] = gold_verdicts([gold], [], {}, TODAY)
    assert (empty.status, empty.label, empty.share, empty.split) == ("open", None, 0.0, {})


def test_no_scorer_output_for_gold_carries_its_known_answer_unless_the_crowd_gave_it() -> None:
    """Property: whatever the votes, a gold row's label is the crowd's
    heaviest answer (or nothing), so it can only ever repeat what players
    said, never the answer key."""
    rng = random.Random(17)
    labels = ["delivery", "soliciting", "visitation", "unknown"]
    for _ in range(300):
        gold = _item(1, gold=rng.choice(labels))
        votes = [_vote(f"p{n}", gold, rng.choice(labels)) for n in range(rng.randint(0, 12))]
        trust = {v.player_id: rng.uniform(0, 2) for v in votes}
        [row] = gold_verdicts([gold], votes, trust, TODAY)
        crowd = verdict(gold, votes, trust, TODAY)
        assert (row.status, row.label, row.split) == (crowd.status, crowd.label, crowd.split)
        if row.label is not None:
            assert row.split[row.label] == max(row.split.values())
