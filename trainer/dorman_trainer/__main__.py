"""dorman-trainer: the nightly intent model.

    dorman-scorer
    dorman-ingest export /var/lib/dorman/export
    dorman-trainer /var/lib/dorman/export /var/lib/dorman/models

Each run that trains writes `models/<run>/` (model.joblib, metrics.json and
the export manifest it learned from). `models/current` points at the model
to use, and moves only when a new model scores at least as well as the
current one on the same held-out rows. Nothing to train on, or no change in
the data since the current model, is a normal night: exit 0. Only real
errors exit non-zero, so cron mail always means something broke.

model.joblib is a pickle: load only files this job wrote.
"""

from __future__ import annotations

import argparse
import json
import logging
import os
from datetime import UTC, datetime
from pathlib import Path

import joblib

from dorman_trainer import data, model

log = logging.getLogger("dorman_trainer")

CURRENT = "current"


def current_dir(models_dir: Path) -> Path | None:
    link = models_dir / CURRENT
    return link.resolve() if link.exists() else None


def promote(models_dir: Path, run_dir: Path) -> None:
    """Points `current` at `run_dir` in one rename, so a reader never finds
    it missing or half-moved."""
    tmp = models_dir / f".{CURRENT}.tmp"
    if tmp.is_symlink() or tmp.exists():
        tmp.unlink()
    tmp.symlink_to(run_dir.name)
    os.replace(tmp, models_dir / CURRENT)


def new_run_dir(models_dir: Path, now: datetime) -> Path:
    """A fresh directory named for the run's time, with a suffix if a run
    already started that second (a manual run right after the nightly one)."""
    stamp = now.strftime("%Y-%m-%dT%H%M%SZ")
    for n in range(1, 100):
        run_dir = models_dir / (stamp if n == 1 else f"{stamp}-{n}")
        try:
            run_dir.mkdir()
            return run_dir
        except FileExistsError:
            continue
    raise RuntimeError(f"too many runs at {stamp}")


def run(
    export_dir: Path,
    models_dir: Path,
    *,
    min_agreement: float = 0.8,
    test_share: float = 0.2,
    min_train: int = 20,
    now: datetime | None = None,
) -> dict:
    """One night's run. Returns what happened, for the log and for tests."""
    now = now or datetime.now(UTC)
    manifest = data.read_manifest(export_dir)
    sha = manifest["files"]["intent.jsonl"]["sha256"]

    current = current_dir(models_dir)
    if current is not None:
        trained_on = json.loads((current / "metrics.json").read_text())["intent_sha256"]
        if trained_on == sha:
            return {"outcome": "unchanged", "current": current.name}

    examples = data.load(export_dir, min_agreement)
    train, test = data.split(examples, test_share)
    labels = {e.label for e in train}
    if len(train) < min_train or len(labels) < 2 or not test:
        return {
            "outcome": "not_enough_data",
            "train": len(train),
            "test": len(test),
            "labels": len(labels),
        }

    new = model.train(train)
    scores = model.evaluate(new, test)
    previous = None
    if current is not None:
        previous = model.evaluate(joblib.load(current / "model.joblib"), test)["macro_f1"]
    promoted = previous is None or scores["macro_f1"] >= previous

    models_dir.mkdir(parents=True, exist_ok=True)
    run_dir = new_run_dir(models_dir, now)
    joblib.dump(new, run_dir / "model.joblib")
    (run_dir / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    metrics = {
        **scores,
        "current_macro_f1": previous,
        "promoted": promoted,
        "intent_sha256": sha,
        "train": len(train),
        "test": len(test),
        "min_agreement": min_agreement,
        "trained_at": now.isoformat(),
    }
    (run_dir / "metrics.json").write_text(json.dumps(metrics, indent=2) + "\n")
    if promoted:
        promote(models_dir, run_dir)
    return {"outcome": "promoted" if promoted else "kept", "run": run_dir.name, **metrics}


def main(argv: list[str] | None = None) -> int:
    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s: %(message)s")
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("export_dir", type=Path, help="a dorman-ingest export directory")
    parser.add_argument("models_dir", type=Path, help="where runs and `current` live")
    parser.add_argument(
        "--min-agreement",
        type=float,
        default=0.8,
        help="least share of trust behind a crowd label to train on it",
    )
    parser.add_argument(
        "--test-share", type=float, default=0.2, help="share of non-gold rows held out"
    )
    parser.add_argument(
        "--min-train", type=int, default=20, help="fewest training rows worth a model"
    )
    args = parser.parse_args(argv)
    result = run(
        args.export_dir,
        args.models_dir,
        min_agreement=args.min_agreement,
        test_share=args.test_share,
        min_train=args.min_train,
    )
    match result["outcome"]:
        case "unchanged":
            log.info("no new labels since %s; nothing to train", result["current"])
        case "not_enough_data":
            log.info(
                "not enough data yet: %d training rows over %d labels, %d test rows",
                result["train"],
                result["labels"],
                result["test"],
            )
        case "promoted":
            log.info("promoted %s: macro-F1 %.3f", result["run"], result["macro_f1"])
        case "kept":
            log.info(
                "kept %s but didn't promote it: macro-F1 %.3f against the current %.3f",
                result["run"],
                result["macro_f1"],
                result["current_macro_f1"],
            )
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
