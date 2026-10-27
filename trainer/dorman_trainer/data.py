"""Reading an export, choosing the rows worth learning from, and splitting
them into training and test sets the same way every night."""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Example:
    id: str
    text: str
    label: str
    gold: bool
    # How much the row counts in training: the crowd's agreement on its
    # label, or 1.0 for gold, whose label is known.
    weight: float


def read_manifest(export_dir: Path) -> dict:
    return json.loads((export_dir / "manifest.json").read_text())


def features(text: str, scene: str | None) -> str:
    """What the model reads: the scene, if any, then what the visitor said."""
    return f"{scene} || {text}" if scene else text


def load(export_dir: Path, min_agreement: float) -> list[Example]:
    """The intent rows confident enough to learn from. Gold always is; a
    crowd label needs at least `min_agreement` of the trust behind it."""
    examples = []
    for line in (export_dir / "intent.jsonl").read_text().splitlines():
        row = json.loads(line)
        gold = row["source"] == "gold"
        agreement = row["agreement"]
        if not gold and (agreement is None or agreement < min_agreement):
            continue
        examples.append(
            Example(
                id=row["id"],
                text=features(row["text"], row["scene"]),
                label=row["label"],
                gold=gold,
                weight=1.0 if gold else agreement,
            )
        )
    return examples


def in_test(example_id: str, test_share: float) -> bool:
    """A fixed coin flip per item, so an item stays on the same side of the
    split from one night to the next and no model is tested on what it
    trained on the night before."""
    digest = hashlib.sha256(example_id.encode()).digest()
    return int.from_bytes(digest[:8], "big") / 2**64 < test_share


def split(examples: list[Example], test_share: float) -> tuple[list[Example], list[Example]]:
    """(train, test). Gold always goes to test: its labels are known, so it
    is the fairest yardstick, and it keeps the model from simply memorising
    the answer key."""
    train, test = [], []
    for example in examples:
        (test if example.gold or in_test(example.id, test_share) else train).append(example)
    return train, test
