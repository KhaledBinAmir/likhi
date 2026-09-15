from likhi.eval.datasets import _strip_punct


def test_strip_punct_keeps_bengali_vowel_signs_and_virama():
    assert _strip_punct("কিন্তু,") == "কিন্তু"
    assert _strip_punct("চলে।") == "চলে"
    assert _strip_punct("(দিয়ে)") == "দিয়ে"
    assert _strip_punct("করছি?") == "করছি"
    assert _strip_punct("বড়…") == "বড়"  # final nukta (decomposed) must survive
    assert _strip_punct("কাঁ!") == "কাঁ"  # candrabindu must survive


def test_strip_punct_latin_and_symbols():
    assert _strip_punct("...ok!!") == "ok"
    assert _strip_punct("'quoted'") == "quoted"
    assert _strip_punct("http://x") == "http://x"  # only edges are touched
