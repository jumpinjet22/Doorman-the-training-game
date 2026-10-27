"""The scorer against simulated crowds. Thresholds come from the completion
plan (M3): settled items are right at least 95% of the time, and trolls end
up with trust of 0.05 or less. A scorer change that breaks either fails CI.
"""

import pytest

from dorman_scorer.scoring import score
from tests.sim import Crowd, simulate

pytestmark = pytest.mark.sim


def _run(crowd: Crowd):
    world = simulate(crowd)
    players, verdicts = score(world.items, world.votes, world.today)
    settled = [v for v in verdicts if v.status == "settled"]
    accuracy = sum(v.label == world.truth[v.item_id] for v in settled) / max(1, len(settled))
    return world, players, verdicts, settled, accuracy


@pytest.mark.parametrize(
    "crowd",
    [
        Crowd(honest=20),
        Crowd(honest=20, trolls=5),  # 20% trolls
        Crowd(honest=30, trolls=20),  # 40% trolls
        Crowd(honest=20, lazy=10, trolls=5),
        Crowd(honest=60, honest_accuracy=0.9, trolls=20, seed=3),
    ],
    ids=["honest-only", "20pct-trolls", "40pct-trolls", "lazy-and-trolls", "big"],
)
def test_settled_items_are_right_and_trolls_are_muted(crowd: Crowd) -> None:
    world, players, verdicts, settled, accuracy = _run(crowd)

    assert len(settled) >= len(verdicts) * 0.5, "most items should settle with this crowd"
    assert accuracy >= 0.95
    for player, kind in world.player_kind.items():
        if kind in ("troll", "lazy"):
            assert players[player].trust <= 0.05, f"{player} kept trust {players[player].trust}"


def test_a_tiny_crowd_settles_nothing_rather_than_guessing() -> None:
    _, _, verdicts, settled, _ = _run(Crowd(honest=3, days=2))
    assert settled == []
    assert verdicts


def test_a_noisy_crowd_never_settles_anything_wrong() -> None:
    _, _, _, settled, accuracy = _run(Crowd(honest=60, honest_accuracy=0.75, trolls=20, seed=3))
    assert settled and accuracy >= 0.95


@pytest.mark.xfail(
    strict=True,
    reason="M3: 75%-accurate players can't reach the 80% weighted share needed to "
    "settle, even when Dawid-Skene is confident. Settle on the posterior instead.",
)
def test_a_noisy_but_honest_crowd_still_settles_most_items() -> None:
    _, _, verdicts, settled, _ = _run(Crowd(honest=60, honest_accuracy=0.75, seed=3))
    assert len(settled) >= len(verdicts) * 0.5


@pytest.mark.xfail(strict=True, reason="brigade detection is milestone M3")
def test_a_brigade_of_new_accounts_cannot_settle_a_wrong_answer() -> None:
    world, _, verdicts, _, _ = _run(Crowd(honest=4, brigades=2, brigade_size=40))
    wrong = [v for v in verdicts if v.status == "settled" and v.label != world.truth[v.item_id]]
    assert wrong == []
