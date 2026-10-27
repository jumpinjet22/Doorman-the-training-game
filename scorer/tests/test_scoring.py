from datetime import date, timedelta

import numpy as np

from dorman_scorer.consensus import consensus_file, verdict
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
        knock_key=f"k{n}",
        mode="intent",
        gold_answer=gold,
        generator_intent="delivery",
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


def test_crowd_contradicting_a_confident_judge_is_disputed() -> None:
    item = _item(1, judge_category_correct=9.0)
    votes = [_vote(f"p{n}", item, "soliciting") for n in range(10)]
    assert verdict(item, votes, {f"p{n}": 1.0 for n in range(10)}, TODAY).status == "disputed"


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


def test_consensus_file_matches_what_knock_imports() -> None:
    item = _item(1)
    v = verdict(item, [_vote("p", item, "delivery")], {"p": 6.0}, TODAY)
    doc = consensus_file(TODAY, [v])
    assert doc["format"] == "knock-game-consensus" and doc["version"] == 1
    assert doc["items"] == [
        {
            "knock_key": "k1",
            "mode": "intent",
            "status": "settled",
            "label": "delivery",
            "share": 1.0,
            "total_weight": 6.0,
        }
    ]
