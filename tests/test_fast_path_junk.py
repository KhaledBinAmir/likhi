"""The list shown while typing is a list you can select from, so it must not contain junk.

The fast path runs without the transliteration model, on trie channels alone. The aligned
romanization data has a tail of misaligned pairs, and one of those plus a high unigram count was
enough to reach the visible list with nothing to contradict it: "কোন" and "হিসেবে" appeared for
bangla, "খনির" for sonar, each attested exactly once and matching neither phonetic key. Pressing 4
committed a word the user never typed.

Reported from the pilot as a screenshot of the picker, which is the only place it shows: the full
path ranks these away, so every offline measurement looked fine.
"""

import pytest

from likhi.engine.core import LikhiEngine


@pytest.fixture(scope="module")
def engine():
    try:
        return LikhiEngine()
    except Exception as exc:  # pragma: no cover - only when the lexicon has not been built
        pytest.skip(f"engine unavailable: {exc}")


@pytest.mark.parametrize(
    "roman, banned",
    [
        ("bangla", ["কোন", "হিসেবে", "থাকলে"]),
        ("sonar", ["খনির"]),
    ],
)
def test_single_misaligned_pairs_do_not_reach_the_visible_list(engine, roman, banned):
    shown, _ = engine.fast_suggest(roman, (), 5)
    for word in banned:
        assert word not in shown, f"{word!r} still shown for {roman!r}: {shown}"


def test_the_right_answer_is_still_first(engine):
    for roman, expected in [("bangla", "বাংলা"), ("sonar", "সোনার"), ("amar", "আমার")]:
        shown, _ = engine.fast_suggest(roman, (), 5)
        assert shown[0] == expected, f"{roman!r} -> {shown}"


def test_weak_evidence_words_still_get_a_full_list(engine):
    """Demotion, not removal: when nothing is well attested the candidates must all survive.

    "khacche" has no attested romanization at all, only phonetic-key matches, so the rule must not
    engage and the list must stay full.
    """
    shown, strong = engine.fast_suggest("khacche", (), 5)
    assert strong is False
    assert len(shown) == 5


def test_demotion_only_engages_when_something_is_well_attested(engine):
    """A rare word typed exactly must not lose its candidates to this rule."""
    shown, _ = engine.fast_suggest("audit", (), 5)
    assert len(shown) == 5
