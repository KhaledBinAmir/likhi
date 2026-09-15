"""Engine smoke tests. They need the built artifacts (models/), so they skip when those are absent."""

from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[1]
LEX = REPO / "models" / "lexicon" / "unigrams.marisa"
XLIT = REPO / "models" / "indicxlit-np" / "model.npz"

pytestmark = pytest.mark.skipif(not (LEX.exists() and XLIT.exists()), reason="models not built")


@pytest.fixture(scope="module")
def engine():
    from likhi.engine.core import LikhiEngine

    return LikhiEngine()


@pytest.mark.parametrize(
    "roman,expected",
    [("amar", "আমার"), ("amr", "আমার"), ("aamar", "আমার"), ("korchi", "করছি"), ("korci", "করছি"), ("tumi", "তুমি")],
)
def test_top1_common_words(engine, roman, expected):
    assert engine.suggest(roman, k=5)[0] == expected


def test_alternates_present(engine):
    cands = engine.suggest("jonno", k=5)
    assert "জন্য" in cands


def test_case_insensitive(engine):
    assert engine.suggest("Amar", k=3) == engine.suggest("amar", k=3)


def test_unseen_name_gets_something(engine):
    cands = engine.suggest("khaled", k=5)
    assert cands and all(c for c in cands)
