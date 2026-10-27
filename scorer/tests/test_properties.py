"""Hypothesis properties for the scorer's math."""

from datetime import date, timedelta

import numpy as np
from hypothesis import given, settings
from hypothesis import strategies as st

from dorman_scorer.brigade import brigaded
from dorman_scorer.config import Config
from dorman_scorer.consensus import verdict
from dorman_scorer.dawid_skene import dawid_skene
from dorman_scorer.model import Item, Vote
from dorman_scorer.scoring import score
from dorman_scorer.trust import gold_trust

LABELS = ["a", "b", "c"]
votes_strategy = st.lists(
    st.tuples(
        st.sampled_from([f"p{n}" for n in range(6)]),
        st.sampled_from([f"i{n}" for n in range(5)]),
        st.sampled_from(LABELS),
    ),
    min_size=1,
    max_size=60,
)


@given(votes_strategy)
def test_posteriors_are_probabilities(votes) -> None:
    result = dawid_skene(votes, LABELS)
    for posterior in result.posteriors.values():
        assert np.all(posterior >= 0)
        assert abs(posterior.sum() - 1.0) < 1e-9
    assert all(0.0 <= acc <= 1.0 for acc in result.accuracy.values())


@given(votes_strategy, st.sampled_from(LABELS))
def test_gold_items_stay_clamped(votes, answer) -> None:
    item = votes[0][1]
    result = dawid_skene(votes, LABELS, gold={item: answer})
    assert result.posteriors[item][LABELS.index(answer)] == 1.0


@settings(max_examples=50)
@given(votes_strategy, st.randoms(use_true_random=False))
def test_vote_order_does_not_matter(votes, rnd) -> None:
    shuffled = list(votes)
    rnd.shuffle(shuffled)
    a, b = dawid_skene(votes, LABELS), dawid_skene(shuffled, LABELS)
    for item, posterior in a.posteriors.items():
        assert np.allclose(posterior, b.posteriors[item], atol=1e-6)


@given(st.integers(0, 200), st.integers(0, 200))
def test_gold_trust_is_bounded_and_rewards_right_answers(right, extra) -> None:
    total = right + extra
    trust = gold_trust(right, total)
    assert 0.0 < trust < 1.0
    assert gold_trust(right + 1, total + 1) > trust


ITEM = Item("i", "k", "intent", None, "a", first_shown=date(2026, 10, 27))


@given(
    st.lists(st.tuples(st.sampled_from(LABELS), st.floats(0.01, 1.0)), min_size=1, max_size=30),
    st.floats(0.01, 1.0),
)
def test_another_vote_for_the_leader_never_lowers_its_share(ballots, weight) -> None:
    votes = [Vote(f"p{n}", "i", label, 3000) for n, (label, _) in enumerate(ballots)]
    trust = {f"p{n}": w for n, (_, w) in enumerate(ballots)}
    before = verdict(ITEM, votes, trust, ITEM.first_shown)
    votes.append(Vote("new", "i", before.label, 3000))
    trust["new"] = weight
    after = verdict(ITEM, votes, trust, ITEM.first_shown)
    assert after.label == before.label
    assert after.share >= before.share - 1e-9


# Whole scoring passes over small random worlds: a few gold items, a few open
# ones, players old and new.
_GOLD = [Item(f"g{n}", f"kg{n}", "intent", LABELS[n % 3], "a") for n in range(4)]
_OPEN = [
    Item(f"i{n}", f"k{n}", "intent", None, "a", first_shown=date(2026, 10, 27)) for n in range(4)
]
_BY_ID = {i.id: i for i in _GOLD + _OPEN}
world_votes = st.lists(
    st.tuples(
        st.sampled_from([f"p{n}" for n in range(14)]),
        st.sampled_from(sorted(_BY_ID)),
        st.sampled_from(LABELS),
        st.booleans(),  # new account
    ),
    max_size=120,
    unique_by=lambda v: (v[0], v[1]),
).map(
    lambda rows: [
        Vote(p, i, a, 3000, account_age=timedelta(hours=2 if new else 900)) for p, i, a, new in rows
    ]
)


def _key(players, verdicts):
    return (
        {p: round(s.trust, 9) for p, s in players.items()},
        [(v.item_id, v.status, v.label, round(v.share, 9), v.zeroed_votes) for v in verdicts],
    )


@settings(max_examples=60, deadline=None)
@given(world_votes, st.randoms(use_true_random=False))
def test_shuffling_votes_does_not_change_the_scores(votes, rnd) -> None:
    shuffled = list(votes)
    rnd.shuffle(shuffled)
    today = ITEM.first_shown
    assert _key(*score(_GOLD + _OPEN, votes, today)) == _key(*score(_GOLD + _OPEN, shuffled, today))


@settings(max_examples=60, deadline=None)
@given(world_votes, st.floats(0.0, 1.0))
def test_established_votes_are_never_zeroed(votes, weight) -> None:
    trust = {f"p{n}": weight for n in range(14)}
    zeroed = brigaded(_BY_ID, votes, trust, Config(brigade_min_votes=2))
    for vote in votes:
        if vote.player_id in zeroed.get(vote.item_id, ()):
            assert vote.account_age < Config().new_account_age
            assert _BY_ID[vote.item_id].gold_answer is None


@settings(max_examples=60, deadline=None)
@given(world_votes)
def test_settled_items_have_the_weight_and_a_majority(votes) -> None:
    config = Config(min_weight=0.5)
    _, verdicts = score(_GOLD + _OPEN, votes, ITEM.first_shown, config)
    for v in verdicts:
        assert abs(sum(v.split.values()) - v.total_weight) < 1e-3
        if v.status == "settled":
            assert v.total_weight >= config.min_weight
            assert v.share >= min(config.min_share, config.min_posterior_share)
            assert v.share >= config.min_share or (v.posterior or 0) >= config.min_posterior


@given(
    st.lists(st.tuples(st.sampled_from(LABELS), st.floats(0.01, 1.0)), min_size=1, max_size=30),
    st.sampled_from(LABELS),
    st.floats(0.01, 1.0),
)
def test_an_honest_vote_never_lowers_the_true_labels_share(ballots, truth, weight) -> None:
    votes = [Vote(f"p{n}", "i", label, 3000) for n, (label, _) in enumerate(ballots)]
    trust = {f"p{n}": w for n, (_, w) in enumerate(ballots)}

    def true_share() -> float:
        v = verdict(ITEM, votes, trust, ITEM.first_shown)
        return v.split.get(truth, 0.0) / v.total_weight

    before = true_share()
    votes.append(Vote("honest", "i", truth, 3000))
    trust["honest"] = weight
    assert true_share() >= before - 1e-9
