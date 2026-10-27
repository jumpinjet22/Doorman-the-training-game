"""A small synthetic export: each intent has its own telltale phrases, so a
baseline can learn them, and gold rows are mixed in as in the real thing."""

from __future__ import annotations

import hashlib
import json
import random
from pathlib import Path

import pytest

PHRASES = {
    "delivery": ["package for you", "parcel at the door", "your order is here"],
    "food_delivery": ["your pizza", "food order", "dinner delivery"],
    "religious_soliciting": ["talk about our church", "a word about faith", "our congregation"],
    "person_lookup": ["is Mark here", "looking for Sarah", "does Tom live here"],
}
OPENERS = ["hi", "hello", "hey there", "excuse me", "good evening", "um hi"]


def rows(n: int, seed: int = 0, shuffle_labels: bool = False) -> list[dict]:
    rng = random.Random(seed)
    labels = list(PHRASES)
    out = []
    for i in range(n):
        label = labels[i % len(labels)]
        text = f"{rng.choice(OPENERS)}, {rng.choice(PHRASES[label])}"
        gold = i % 7 == 0
        out.append(
            {
                "id": f"item{i}",
                "text": text,
                "scene": "night, porch light on" if i % 5 == 0 else None,
                "label": rng.choice(labels) if shuffle_labels and not gold else label,
                "source": "gold" if gold else "crowd",
                "intended_intent": label,
                "agreement": None if gold else 0.6 + 0.4 * rng.random(),
                "weight": None if gold else 5.0,
                "votes": 0 if gold else 9,
                "batch": "b1",
            }
        )
    return out


def write_export(directory: Path, intent_rows: list[dict]) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    text = "".join(json.dumps(r) + "\n" for r in intent_rows)
    (directory / "intent.jsonl").write_text(text)
    (directory / "dpo.jsonl").write_text("")
    manifest = {
        "format": "dorman-export",
        "version": 1,
        "files": {
            "intent.jsonl": {
                "rows": len(intent_rows),
                "sha256": hashlib.sha256(text.encode()).hexdigest(),
            },
            "dpo.jsonl": {"rows": 0, "sha256": hashlib.sha256(b"").hexdigest()},
        },
    }
    (directory / "manifest.json").write_text(json.dumps(manifest))
    return directory


@pytest.fixture
def export(tmp_path: Path) -> Path:
    return write_export(tmp_path / "export", rows(200))
