"""The scorer against simulated crowds. Thresholds come from the completion
plan (M3): settled items are right at least 95% of the time, and trolls end
up with trust of 0.05 or less. A scorer change that breaks either fails CI.
"""

import random
from datetime import timedelta

import pytest

from dorman_scorer.cohort import cohorts
from dorman_scorer.config import SOFT_LAUNCH, Config
from dorman_scorer.model import Vote
from dorman_scorer.scoring import score
from tests.sim import LABELS, Crowd, simulate

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


def test_a_noisy_but_honest_crowd_still_settles_most_items() -> None:
    _, _, verdicts, settled, accuracy = _run(Crowd(honest=60, honest_accuracy=0.75, seed=3))
    assert len(settled) >= len(verdicts) * 0.5
    assert accuracy >= 0.95


def test_gold_answered_in_overtime_earns_no_trust() -> None:
    """A farmer trolls their dailies but answers every gold item they can
    reach in Overtime correctly. Only daily gold counts, so they're muted."""
    world = simulate(Crowd(honest=20, days=2))
    rng = random.Random(7)
    farmer = "farmer"
    seen = set()
    for vote in [v for v in world.votes if v.player_id == "honest0"]:
        item = next(i for i in world.items if i.id == vote.item_id)
        right = item.gold_answer or world.truth[item.id]
        # Wrong, but varied and at a human pace, so no behaviour flag trips.
        ans = rng.choice([label for label in LABELS if label != right])
        world.votes.append(Vote(farmer, item.id, ans, rng.randint(3000, 9000), rng.randint(0, 3)))
        seen.add(item.id)
    overtime_gold = [i for i in world.items if i.gold_answer and i.id not in seen]
    assert len(overtime_gold) >= 5
    for item in overtime_gold:
        world.votes.append(
            Vote(farmer, item.id, item.gold_answer, rng.randint(3000, 9000), 0, daily=False)
        )

    players, _ = score(world.items, world.votes, world.today)
    assert players[farmer].trust <= 0.05, f"the farmer kept trust {players[farmer].trust}"
    assert players[farmer].flags == {}, "muted by the gold filter, not a behaviour flag"


def test_a_brigade_of_new_accounts_cannot_settle_a_wrong_answer() -> None:
    world, _, verdicts, _, _ = _run(Crowd(honest=4, brigades=2, brigade_size=40))
    wrong = [v for v in verdicts if v.status == "settled" and v.label != world.truth[v.item_id]]
    assert wrong == []


def test_honest_newcomers_are_not_mistaken_for_a_brigade() -> None:
    world, _, verdicts, settled, accuracy = _run(Crowd(honest=20, newcomers=30))
    assert all(v.zeroed_votes == 0 for v in verdicts)
    assert len(settled) == len(verdicts) and accuracy == 1.0


def test_a_launch_day_crowd_settles_under_soft_launch_only() -> None:
    # Launch day: every account is hours old, so every vote is a new-account vote.
    world = simulate(Crowd(honest=0, newcomers=25, days=1))

    _, held = score(world.items, world.votes, world.today, Config())
    assert {(v.status, v.zeroed_votes) for v in held} == {("open", 25)}
    later = world.today + timedelta(days=3)
    _, still_held = score(world.items, world.votes, later, Config())
    assert {v.status for v in still_held} == {"disputed"}

    _, verdicts = score(world.items, world.votes, world.today, SOFT_LAUNCH)
    assert all(v.status == "settled" and v.zeroed_votes == 0 for v in verdicts)
    assert all(v.label == world.truth[v.item_id] for v in verdicts)


# The M3 grid: 10-40% trolls, 0-3 brigades, 20-500 players.
GRID = [
    (players, pct, brigades)
    for players in (20, 100, 500)
    for pct in (10, 20, 30, 40)
    for brigades in (0, 1, 2, 3)
]


def _grid_crowd(players: int, pct: int, brigades: int, accuracy: float) -> Crowd:
    trolls = round(players * pct / 100)
    return Crowd(
        honest=players - trolls,
        honest_accuracy=accuracy,
        trolls=trolls,
        brigades=brigades,
        seed=players + pct + brigades,
    )


def _assert_thresholds(world, players, settled, accuracy) -> None:
    assert accuracy >= 0.95 or not settled, f"settled accuracy {accuracy:.3f}"
    for player, kind in world.player_kind.items():
        if kind in ("troll", "lazy"):
            assert players[player].trust <= 0.05, f"{player} kept trust {players[player].trust}"


@pytest.mark.parametrize(("players", "pct", "brigades"), GRID, ids=lambda x: str(x))
def test_grid_of_crowds(players: int, pct: int, brigades: int) -> None:
    world, scores, verdicts, settled, accuracy = _run(_grid_crowd(players, pct, brigades, 0.85))
    _assert_thresholds(world, scores, settled, accuracy)
    assert len(settled) >= len(verdicts) * 0.5, "an 85%-right crowd should settle most items"


@pytest.mark.parametrize(("players", "pct", "brigades"), GRID, ids=lambda x: str(x))
def test_grid_of_noisy_crowds(players: int, pct: int, brigades: int) -> None:
    # 75%-right players: too little trust to settle much with 20 players, but
    # whatever settles must still be right.
    world, scores, verdicts, settled, accuracy = _run(_grid_crowd(players, pct, brigades, 0.75))
    _assert_thresholds(world, scores, settled, accuracy)
    if players >= 100:
        assert len(settled) >= len(verdicts) * 0.5


@pytest.mark.slow
@pytest.mark.parametrize("seed", range(5))
def test_large_crowds_over_two_weeks(seed: int) -> None:
    crowd = Crowd(
        honest=1600,
        honest_accuracy=0.75,
        lazy=200,
        trolls=600,
        brigades=3,
        brigade_size=60,
        newcomers=100,
        days=14,
        seed=seed,
    )
    world, scores, verdicts, settled, accuracy = _run(crowd)
    _assert_thresholds(world, scores, settled, accuracy)
    assert len(settled) >= len(verdicts) * 0.5


# -- sleeper farms (pre-aged accounts under the per-item threshold) -----------


def test_a_sleeper_farm_cannot_settle_a_wrong_answer() -> None:
    """Accounts created together two months back, answering harvested gold
    right and never more than seven to an item: neither new nor a brigade,
    but they vote as one, so cohort detection mutes them."""
    world, players, verdicts, settled, accuracy = _run(Crowd(honest=12, sleepers=20))
    wrong = [v for v in settled if v.label != world.truth[v.item_id]]
    assert wrong == []
    assert any(v.zeroed_votes for v in verdicts)
    assert settled and accuracy >= 0.95


@pytest.mark.parametrize("seed", range(3))
def test_a_sleeper_farm_inside_a_bigger_crowd(seed: int) -> None:
    world, players, verdicts, settled, accuracy = _run(
        Crowd(honest=40, trolls=10, newcomers=20, sleepers=30, sleeper_per_item=7, seed=seed)
    )
    assert accuracy >= 0.95
    for player, kind in world.player_kind.items():
        if kind == "troll":
            assert players[player].trust <= 0.05


def test_honest_players_who_joined_together_are_not_a_cohort() -> None:
    """Every regular here signed up within two days of the others, and the
    newcomers within one: agreeing with the crowd most of the time isn't
    voting as one."""
    world, _, verdicts, settled, accuracy = _run(Crowd(honest=60, newcomers=30, days=10))
    assert all(v.zeroed_votes == 0 for v in verdicts)
    assert len(settled) == len(verdicts) and accuracy == 1.0


@pytest.mark.parametrize("accuracy", [0.95, 0.98, 1.0])
def test_a_skilled_crowd_that_joined_together_is_not_a_cohort(accuracy: float) -> None:
    """Launch day: everyone joined within hours, and good players agree on
    nearly everything. Agreeing on the right answer isn't a farm's signature;
    only agreeing against everyone else is."""
    world = simulate(Crowd(honest=40, honest_accuracy=accuracy, days=7))
    assert cohorts({i.id: i for i in world.items}, world.votes, Config()) == []
    _, verdicts = score(world.items, world.votes, world.today)
    assert all(v.zeroed_votes == 0 for v in verdicts)
    assert all(v.status == "settled" for v in verdicts)
    assert all(v.label == world.truth[v.item_id] for v in verdicts)


def test_the_sleeper_farm_is_found_as_one_cohort() -> None:
    world = simulate(Crowd(honest=12, sleepers=20))
    found = cohorts({i.id: i for i in world.items}, world.votes, Config())
    farm = {p for p, kind in world.player_kind.items() if kind == "sleeper"}
    assert found == [farm]
