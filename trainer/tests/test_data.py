from pathlib import Path

from dorman_trainer import data

from .conftest import rows, write_export


def test_the_split_is_the_same_every_night_and_gold_is_always_tested(export: Path):
    examples = data.load(export, min_agreement=0.0)
    train, test = data.split(examples, 0.2)
    again_train, again_test = data.split(list(reversed(examples)), 0.2)
    assert {e.id for e in train} == {e.id for e in again_train}
    assert {e.id for e in test} == {e.id for e in again_test}
    assert not any(e.gold for e in train)
    assert {e.id for e in examples if e.gold} <= {e.id for e in test}
    # Roughly the requested share of non-gold rows is held out.
    crowd = [e for e in examples if not e.gold]
    held = sum(1 for e in crowd if e in test)
    assert 0.1 < held / len(crowd) < 0.3


def test_shaky_crowd_labels_are_dropped_but_gold_never_is(tmp_path: Path):
    export = write_export(tmp_path / "e", rows(70))
    everything = data.load(export, min_agreement=0.0)
    confident = data.load(export, min_agreement=0.9)
    assert len(confident) < len(everything)
    assert {e.id for e in everything if e.gold} == {e.id for e in confident if e.gold}
    assert all(e.weight >= 0.9 for e in confident if not e.gold)
    assert all(e.weight == 1.0 for e in confident if e.gold)


def test_a_crowd_row_with_no_agreement_is_never_trained_on(tmp_path: Path):
    row = rows(2)[1] | {"agreement": None}
    export = write_export(tmp_path / "e", [row])
    assert data.load(export, min_agreement=0.0) == []


def test_the_model_reads_the_scene_before_the_line():
    assert data.features("hello", None) == "hello"
    assert data.features("hello", "night") == "night || hello"
