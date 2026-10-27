import json
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from dorman_trainer.__main__ import main, new_run_dir, promote, run

from .conftest import rows, write_export

NIGHT = datetime(2026, 10, 27, 3, 0, tzinfo=UTC)


def test_the_first_model_is_promoted_and_learns_something(export: Path, tmp_path: Path):
    models = tmp_path / "models"
    result = run(export, models, min_agreement=0.0, now=NIGHT)
    assert result["outcome"] == "promoted"
    assert result["current_macro_f1"] is None
    assert result["macro_f1"] > 0.9
    current = (models / "current").resolve()
    assert current.name == "2026-10-27T030000Z"
    metrics = json.loads((current / "metrics.json").read_text())
    assert metrics["promoted"] is True
    assert set(metrics["per_label"]) == {
        "delivery",
        "food_delivery",
        "religious_soliciting",
        "person_lookup",
    }
    assert (current / "model.joblib").exists()
    assert json.loads((current / "manifest.json").read_text())["format"] == "dorman-export"


def test_the_same_export_twice_trains_nothing(export: Path, tmp_path: Path):
    models = tmp_path / "models"
    run(export, models, min_agreement=0.0, now=NIGHT)
    again = run(export, models, min_agreement=0.0, now=NIGHT + timedelta(days=1))
    assert again == {"outcome": "unchanged", "current": "2026-10-27T030000Z"}
    assert len([p for p in models.iterdir() if p.name != "current"]) == 1


def test_a_worse_model_is_kept_but_not_promoted(export: Path, tmp_path: Path):
    models = tmp_path / "models"
    run(export, models, min_agreement=0.0, now=NIGHT)
    # Tomorrow the crowd's labels are noise; gold, the yardstick, is not.
    noisy = write_export(tmp_path / "noisy", rows(200, seed=1, shuffle_labels=True))
    result = run(noisy, models, min_agreement=0.0, now=NIGHT + timedelta(days=1))
    assert result["outcome"] == "kept"
    assert result["macro_f1"] < result["current_macro_f1"]
    assert (models / "current").resolve().name == "2026-10-27T030000Z"
    assert (models / "2026-10-28T030000Z" / "model.joblib").exists()


def test_a_better_or_equal_model_replaces_the_current_one(export: Path, tmp_path: Path):
    models = tmp_path / "models"
    run(export, models, min_agreement=0.0, now=NIGHT)
    more = write_export(tmp_path / "more", rows(260))
    result = run(more, models, min_agreement=0.0, now=NIGHT + timedelta(days=1))
    assert result["outcome"] == "promoted"
    assert result["macro_f1"] >= result["current_macro_f1"]
    assert (models / "current").resolve().name == "2026-10-28T030000Z"


def test_too_little_data_is_a_quiet_night(tmp_path: Path):
    export = write_export(tmp_path / "e", rows(10))
    result = run(export, tmp_path / "models", min_agreement=0.0, now=NIGHT)
    assert result["outcome"] == "not_enough_data"
    assert not (tmp_path / "models").exists()


def test_promoting_replaces_the_link_in_place(tmp_path: Path):
    for name in ("a", "b"):
        (tmp_path / name).mkdir()
    (tmp_path / ".current.tmp").symlink_to("a")  # left over from a crash
    promote(tmp_path, tmp_path / "a")
    promote(tmp_path, tmp_path / "b")
    assert (tmp_path / "current").resolve().name == "b"
    assert not (tmp_path / ".current.tmp").exists()


def test_every_outcome_exits_zero(export: Path, tmp_path: Path, caplog):
    models = str(tmp_path / "models")
    small = write_export(tmp_path / "small", rows(10))
    noisy = write_export(tmp_path / "noisy", rows(200, seed=1, shuffle_labels=True))
    caplog.set_level("INFO")
    assert main([str(small), models]) == 0
    assert main([str(export), models, "--min-agreement", "0"]) == 0
    assert main([str(export), models]) == 0
    assert main([str(noisy), models, "--min-agreement", "0"]) == 0
    text = caplog.text
    for phrase in ("not enough data", "promoted", "nothing to train", "didn't promote"):
        assert phrase in text, phrase


def test_runs_in_the_same_second_get_their_own_directories(tmp_path: Path):
    first = new_run_dir(tmp_path, NIGHT)
    second = new_run_dir(tmp_path, NIGHT)
    assert (first.name, second.name) == ("2026-10-27T030000Z", "2026-10-27T030000Z-2")
    for n in range(3, 100):
        (tmp_path / f"2026-10-27T030000Z-{n}").mkdir()
    with pytest.raises(RuntimeError):
        new_run_dir(tmp_path, NIGHT)
