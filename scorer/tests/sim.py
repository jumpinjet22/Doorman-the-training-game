"""A crowd simulator for the scorer: a mix of player types answering a run of
daily sets, with gold mixed in the way the real game does it.

Player types:
- honest: right with probability `accuracy`, otherwise a random wrong label
- lazy: random answers, fast
- troll: always the same wrong label, fast
- brigade: brand-new accounts that all push one wrong label on the same
  items, answering gold honestly enough to blend in
"""

from __future__ import annotations

import random
from dataclasses import dataclass, field
from datetime import date, timedelta

from dorman_scorer.model import Item, Vote

LABELS = ["delivery", "food_delivery", "soliciting", "visitation", "person_lookup", "unknown"]
START = date(2026, 10, 1)


@dataclass
class Crowd:
    honest: int = 20
    honest_accuracy: float = 0.85
    lazy: int = 0
    trolls: int = 0
    brigades: int = 0
    brigade_size: int = 15
    days: int = 7
    new_per_day: int = 6
    gold_per_day: int = 3
    gold_pool: int = 15
    seed: int = 0


@dataclass
class World:
    items: list[Item]
    votes: list[Vote]
    truth: dict[str, str]
    player_kind: dict[str, str] = field(default_factory=dict)
    today: date = START


def simulate(crowd: Crowd) -> World:
    rng = random.Random(crowd.seed)
    gold = [
        Item(f"g{n}", f"kg{n}", "intent", rng.choice(LABELS), "delivery", first_shown=START)
        for n in range(crowd.gold_pool)
    ]
    truth: dict[str, str] = {}
    items = list(gold)
    days: list[tuple[date, list[Item]]] = []
    for d in range(crowd.days):
        day = START + timedelta(days=d)
        new = []
        for n in range(crowd.new_per_day):
            label = rng.choice(LABELS)
            item = Item(f"d{d}n{n}", f"kd{d}n{n}", "intent", None, label, first_shown=day)
            truth[item.id] = label
            new.append(item)
        items += new
        days.append((day, new + rng.sample(gold, crowd.gold_per_day)))

    def answer_of(item: Item) -> str:
        return item.gold_answer or truth[item.id]

    def wrong(right: str, r: random.Random) -> str:
        return r.choice([label for label in LABELS if label != right])

    world = World(items, [], truth, today=START + timedelta(days=crowd.days))
    population = (
        [("honest", n) for n in range(crowd.honest)]
        + [("lazy", n) for n in range(crowd.lazy)]
        + [("troll", n) for n in range(crowd.trolls)]
    )
    for kind, n in population:
        player = f"{kind}{n}"
        world.player_kind[player] = kind
        troll_label = rng.choice(LABELS)
        for _, daily in days:
            for item in daily:
                right = answer_of(item)
                if kind == "honest":
                    ans = right if rng.random() < crowd.honest_accuracy else wrong(right, rng)
                    ms = rng.randint(2500, 9000)
                elif kind == "lazy":
                    ans, ms = rng.choice(LABELS), rng.randint(300, 900)
                else:
                    ans, ms = troll_label, rng.randint(300, 900)
                world.votes.append(Vote(player, item.id, ans, ms, rng.randint(0, 3)))

    # Brigades show up on the last day only and hit that day's new items.
    last_day = days[-1][1]
    for b in range(crowd.brigades):
        for n in range(crowd.brigade_size):
            player = f"brigade{b}_{n}"
            world.player_kind[player] = "brigade"
            for item in last_day:
                right = answer_of(item)
                ans = right if item.gold_answer else wrong(right, random.Random(f"{b}{item.id}"))
                world.votes.append(Vote(player, item.id, ans, rng.randint(2500, 6000), None))
    return world
