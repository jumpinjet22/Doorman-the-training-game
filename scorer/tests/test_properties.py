"""Hypothesis properties for the scorer's math."""

from datetime import date

import numpy as np
from hypothesis import given, settings
from hypothesis import strategies as st

from dorman_scorer.consensus import verdict
from dorman_scorer.dawid_skene import dawid_skene
from dorman_scorer.model import Item, Vote
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
