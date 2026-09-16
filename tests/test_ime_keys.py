"""The text service must read the physical key, not what the layout underneath printed.

Regression test for the bug two pilot machines hit: Windows attaches Bengali INSCRIPT to the bn-BD
language, INSCRIPT sits underneath our text service and maps the letter keys onto Bangla letters, so
every key arrived as a non-ASCII character, we declined it, and the application received raw INSCRIPT
output. Users reported it as "Likhi types random Bangla". It went unnoticed because the development
machine's bn-BD layout was substituted with US English, so keys arrived as Latin there.
"""

import importlib.util
import sys
import types
from pathlib import Path

import pytest

IME_PATH = Path(__file__).resolve().parents[1] / "windows" / "pime" / "likhi" / "likhi_ime.py"

# Virtual key codes, as Windows defines them and as PIME's keycodes.py exposes them.
VK = {
    "VK_BACK": 0x08,
    "VK_CAPITAL": 0x14,
    "VK_CONTROL": 0x11,
    "VK_DOWN": 0x28,
    "VK_ESCAPE": 0x1B,
    "VK_F1": 0x70,
    "VK_LEFT": 0x25,
    "VK_MENU": 0x12,
    "VK_OEM_COMMA": 0xBC,
    "VK_OEM_PERIOD": 0xBE,
    "VK_RETURN": 0x0D,
    "VK_RIGHT": 0x27,
    "VK_SHIFT": 0x10,
    "VK_SPACE": 0x20,
    "VK_UP": 0x26,
}


@pytest.fixture(scope="module")
def ime():
    """Import likhi_ime with PIME's two host modules stubbed out.

    The real ones only exist inside PIME's own Python interpreter, so this is the only way to reach
    the key handling from the test suite.
    """
    keycodes = types.ModuleType("keycodes")
    for name, value in VK.items():
        setattr(keycodes, name, value)
    text_service = types.ModuleType("textService")
    text_service.TextService = type("TextService", (), {"__init__": lambda self, client=None: None})
    sys.modules.setdefault("keycodes", keycodes)
    sys.modules.setdefault("textService", text_service)

    spec = importlib.util.spec_from_file_location("likhi_ime", IME_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeKey:
    """One key press. charCode is what the active layout produced, keyCode is the physical key."""

    def __init__(self, keyCode, charCode=0, shift=False, caps=False, ctrl=False, alt=False):
        self.keyCode = keyCode
        self.charCode = charCode
        self._down = {VK["VK_SHIFT"]: shift, VK["VK_CONTROL"]: ctrl, VK["VK_MENU"]: alt}
        self._toggled = {VK["VK_CAPITAL"]: caps}

    def isChar(self):
        return self.charCode != 0

    def isKeyDown(self, code):
        return self._down.get(code, False)

    def isKeyToggled(self, code):
        return self._toggled.get(code, False)


def test_us_layout_is_unchanged(ime):
    """The common case must behave exactly as before: the layout's own character wins."""
    assert ime._roman_char(FakeKey(0x41, ord("a"))) == "a"
    assert ime._roman_char(FakeKey(0x41, ord("A"), shift=True)) == "A"
    assert ime._roman_char(FakeKey(0x35, ord("5"))) == "5"
    assert ime._roman_char(FakeKey(VK["VK_OEM_PERIOD"], ord("."))) == "."


@pytest.mark.parametrize(
    "bangla_char, expected",
    [
        ("আ", "a"),  # INSCRIPT: the A key types আ
        ("ম", "m"),  # the M key types ম
        ("র", "r"),  # the R key types র
    ],
)
def test_inscript_layout_still_yields_the_roman_letter(ime, bangla_char, expected):
    """This is the bug. The layout printed Bangla; the physical key is still a Latin letter."""
    key = FakeKey(ord(expected.upper()), ord(bangla_char))
    assert ime._roman_char(key) == expected


def test_inscript_respects_shift_and_caps(ime):
    inscript_a = ord("আ")
    assert ime._roman_char(FakeKey(0x41, inscript_a)) == "a"
    assert ime._roman_char(FakeKey(0x41, inscript_a, shift=True)) == "A"
    assert ime._roman_char(FakeKey(0x41, inscript_a, caps=True)) == "A"
    # Shift and Caps together cancel out, as they do everywhere else in Windows.
    assert ime._roman_char(FakeKey(0x41, inscript_a, shift=True, caps=True)) == "a"


def test_typing_amar_on_inscript_produces_the_roman_word(ime):
    """End to end over the helper: the whole word survives a non-Latin layout."""
    pressed = [("a", "আ"), ("m", "ম"), ("a", "আ"), ("r", "র")]
    typed = "".join(ime._roman_char(FakeKey(ord(r.upper()), ord(b))) for r, b in pressed)
    assert typed == "amar"


def test_digits_and_punctuation_survive_a_non_latin_layout(ime):
    assert ime._roman_char(FakeKey(0x37, ord("৭"))) == "7"  # Bangla digit ৭ on the 7 key
    assert ime._roman_char(FakeKey(VK["VK_OEM_PERIOD"], ord("।"))) == "."  # danda

def test_shifted_digit_row_is_not_mistaken_for_a_digit(ime):
    """Shift+7 is punctuation, not 7; without a character we must not invent one."""
    assert ime._roman_char(FakeKey(0x37, 0, shift=True)) == ""


def test_non_character_keys_yield_nothing(ime):
    for name in ("VK_RETURN", "VK_ESCAPE", "VK_BACK", "VK_UP", "VK_F1"):
        assert ime._roman_char(FakeKey(VK[name])) == "", name
