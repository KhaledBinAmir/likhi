import unicodedata

from likhi.engine import textnorm as tn

PRECOMPOSED = "বড়"  # ব + ড় (U+09DC)
DECOMPOSED = "বড়"  # ব + ড + nukta (what NFC produces)


def test_nukta_precomposed_and_decomposed_are_equivalent():
    assert tn.equivalent(PRECOMPOSED, DECOMPOSED)
    assert tn.canonical(PRECOMPOSED) == unicodedata.normalize("NFC", PRECOMPOSED) == DECOMPOSED


def test_to_output_recomposes_nukta():
    assert tn.to_output(DECOMPOSED) == PRECOMPOSED
    assert tn.to_output(PRECOMPOSED) == PRECOMPOSED
    assert tn.to_output(DECOMPOSED, precomposed_nukta=False) == DECOMPOSED


def test_candrabindu_moved_after_vowel_sign():
    wrong = "কঁা"  # candrabindu typed before aa-kar
    right = "কাঁ"
    assert tn.canonical(wrong) == right
    assert tn.equivalent(wrong, right)


def test_joiners_ignored_for_matching_but_kept_in_output():
    with_zwj = "র‍্য"
    without = "র্য"
    assert tn.equivalent(with_zwj, without)
    assert "‍" in tn.to_output(with_zwj)


def test_legacy_khanda_ta_upgraded():
    assert tn.canonical("হঠাত্‍") == "হঠাৎ"


def test_digits():
    assert tn.to_bangla_digits("2026") == "২০২৬"
    assert tn.to_western_digits("২০২৬") == "2026"


def test_normalize_roman_is_case_insensitive():
    assert tn.normalize_roman("AMar") == tn.normalize_roman("amar") == "amar"
    assert tn.normalize_roman("Korchi!") == "korchi"
