"""A crowd simulator for the scorer: a mix of player types answering a run of
daily sets, with gold mixed in the way the real game does it.

Player types:
- honest: right with probability `accuracy`, otherwise a random wrong label
- lazy: random answers, fast
- troll: always the same wrong label, fast
- brigade: brand-new accounts that all push one wrong label on the same
  items, answering gold honestly enough to blend in
- newcomer: an honest player whose account is brand new on the last day, so
  brigade detection has honest new accounts to tell apart from brigades
- sleeper: a farm of accounts signed up together long before the run (so
  never "new"), which answers gold right (as if harvested) and pushes one
  wrong label on every other item, never more than `sleeper_per_item` of
  them on any one item so the per-item brigade threshold never trips

Regulars' accounts are `regular_age_days` old when the run starts, all signed
up within two days of each other; brigades and newcomers sign up on the last
day and vote within hours.
"""

from __future__ import annotations

import random
from dataclasses import dataclass, field, replace
from datetime import date, datetime, time, timedelta

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
    newcomers: int = 0
    sleepers: int = 0
    sleeper_per_item: int = 7
    sleeper_age_days: int = 60
    regular_age_days: int = 30
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
    start = datetime.combine(START, time())
    population = (
        [("honest", n) for n in range(crowd.honest)]
        + [("lazy", n) for n in range(crowd.lazy)]
        + [("troll", n) for n in range(crowd.trolls)]
    )
    for kind, n in population:
        player = f"{kind}{n}"
        world.player_kind[player] = kind
        troll_label = rng.choice(LABELS)
        joined = start - timedelta(days=crowd.regular_age_days, hours=rng.uniform(0, 48))
        for d, (_, daily) in enumerate(days):
            for item in daily:
                right = answer_of(item)
                if kind == "honest":
                    ans = right if rng.random() < crowd.honest_accuracy else wrong(right, rng)
                    ms = rng.randint(2500, 9000)
                elif kind == "lazy":
                    ans, ms = rng.choice(LABELS), rng.randint(300, 900)
                else:
                    ans, ms = troll_label, rng.randint(300, 900)
                age = timedelta(days=crowd.regular_age_days + d)
                world.votes.append(
                    Vote(player, item.id, ans, ms, rng.randint(0, 3), age, joined=joined)
                )

    def new_account() -> timedelta:
        return timedelta(hours=rng.uniform(0, 24))

    last_start = datetime.combine(days[-1][0], time())

    farm = [f"sleeper{n}" for n in range(crowd.sleepers)]
    farm_start = start - timedelta(days=crowd.sleeper_age_days)
    farm_joined = {p: farm_start + timedelta(hours=rng.uniform(0, 12)) for p in farm}
    for d, (_, daily) in enumerate(days):
        for item in daily if farm else []:
            right = answer_of(item)
            push = right if item.gold_answer else wrong(right, random.Random(f"s{item.id}"))
            for player in rng.sample(farm, min(crowd.sleeper_per_item, len(farm))):
                world.player_kind[player] = "sleeper"
                age = timedelta(days=crowd.sleeper_age_days + d)
                vote = Vote(player, item.id, push, rng.randint(2500, 9000), rng.randint(0, 3), age)
                world.votes.append(replace(vote, joined=farm_joined[player]))

    last_day = days[-1][1]
    for n in range(crowd.newcomers):
        player = f"newcomer{n}"
        world.player_kind[player] = "newcomer"
        for item in last_day:
            right = answer_of(item)
            ans = right if rng.random() < crowd.honest_accuracy else wrong(right, rng)
            ms, position = rng.randint(2500, 9000), rng.randint(0, 3)
            age = new_account()
            world.votes.append(
                Vote(player, item.id, ans, ms, position, age, joined=last_start + age / 2)
            )

    # Brigades show up on the last day only and hit that day's new items.
    for b in range(crowd.brigades):
        for n in range(crowd.brigade_size):
            player = f"brigade{b}_{n}"
            world.player_kind[player] = "brigade"
            for item in last_day:
                right = answer_of(item)
                ans = right if item.gold_answer else wrong(right, random.Random(f"{b}{item.id}"))
                age = new_account()
                world.votes.append(
                    Vote(
                        player,
                        item.id,
                        ans,
                        rng.randint(2500, 6000),
                        None,
                        age,
                        joined=last_start + age / 2,
                    )
                )
    return world
