from likhi.eval import metrics as m


def test_edit_distance_basic():
    assert m.edit_distance("kitten", "sitting") == 3
    assert m.edit_distance("", "abc") == 3
    assert m.edit_distance("abc", "abc") == 0


def test_rank_with_alternate_golds():
    cands = ["জন্যে", "জন্য", "জনো"]
    assert m.rank_of_gold(cands, ["জন্য"]) == 2
    assert m.rank_of_gold(cands, ["জন্য", "জন্যে"]) == 1
    assert m.rank_of_gold(cands, ["কিছু"]) is None


def test_word_eval_weighted_summary():
    ev = m.WordEval()
    ev.add(["আমার", "আমরা"], ["আমার"], weight=3)
    ev.add(["আমরা", "আমার"], ["আমার"], weight=1)
    ev.add(["কিছু"], ["করছি"], weight=1)
    s = ev.summary()
    assert s["n"] == 3
    assert abs(s["top1"] - 60.0) < 1e-9  # 3 of 5 weight
    assert abs(s["top3"] - 80.0) < 1e-9
    assert 0 < s["cer"] < 100


def test_cer_ignores_nukta_encoding():
    assert m.cer("বড়", "বড়") == 0.0


def test_percentile():
    assert m.percentile([5, 1, 3], 50) == 3
    assert m.percentile([5, 1, 3], 100) == 5
