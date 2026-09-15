from likhi.engine.personal import PersonalStore


def test_learn_and_recall(tmp_path):
    ps = PersonalStore(tmp_path / "p.sqlite")
    assert ps.selections("amr") == {}
    ps.learn("amr", "আমার", when=1_000_000.0)
    ps.learn("amr", "আমার", when=1_000_100.0)
    ps.learn("amr", "আমরা", when=1_000_200.0)
    sel = ps.selections("amr", now=1_000_200.0)
    assert sel["আমার"] > sel["আমরা"] > 0
    assert ps.word_count("আমার", now=1_000_200.0) > 1.9


def test_case_and_normalization(tmp_path):
    ps = PersonalStore(tmp_path / "p.sqlite")
    ps.learn("Amr", "আমার", when=0.0)
    assert "আমার" in ps.selections("amr", now=0.0)


def test_latin_passthrough_learned(tmp_path):
    ps = PersonalStore(tmp_path / "p.sqlite")
    ps.learn("ok", "ok", when=0.0)
    assert ps.selections("ok", now=0.0) == {"ok": 1.0}


def test_decay_halves_after_half_life(tmp_path):
    ps = PersonalStore(tmp_path / "p.sqlite", half_life_days=1.0)
    ps.learn("ki", "কি", when=0.0)
    assert abs(ps.selections("ki", now=86400.0)["কি"] - 0.5) < 1e-6


def test_forget(tmp_path):
    ps = PersonalStore(tmp_path / "p.sqlite")
    ps.learn("ki", "কি", when=0.0)
    ps.forget(word="কি")
    assert ps.selections("ki", now=0.0) == {}
    assert ps.word_count("কি", now=0.0) == 0.0
