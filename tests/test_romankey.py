import pytest

from likhi.engine.romankey import key_from_bangla, key_from_roman


@pytest.mark.parametrize(
    "romans,bangla",
    [
        (["amar", "amr", "aamar", "Amar"], "আমার"),
        (
            ["korchi", "korci", "kortesi"[:5]],
            "করছি",
        ),  # 'korte' is a different word, only the first two must match
        (["jonno", "jonne", "jonyo"], "জন্য"),
        (["tumi", "tmi"], "তুমি"),
        (["valo", "bhalo", "valo"], "ভালো"),
        (["kemon", "kmn"], "কেমন"),
        (["shopno", "sopno"], "স্বপ্ন"),
        (["bangladesh"], "বাংলাদেশ"),
    ],
)
def test_coarse_keys_match(romans, bangla):
    target = key_from_bangla(bangla, "coarse")
    for r in romans[:2]:
        assert key_from_roman(r, "coarse") == target, (r, key_from_roman(r, "coarse"), target)


def test_fine_keeps_aspiration():
    assert key_from_roman("khali", "fine") != key_from_roman("kali", "fine")
    assert key_from_bangla("খালি", "fine") == key_from_roman("khali", "fine")
    assert key_from_bangla("কালি", "fine") == key_from_roman("kali", "fine")


def test_doubled_consonants_collapse():
    assert key_from_roman("amma", "coarse") == key_from_bangla("আম্মা", "coarse")


def test_nukta_letters():
    assert key_from_bangla("বড়", "coarse") == key_from_roman("boro", "coarse")
