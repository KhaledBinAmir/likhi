"""Systems under evaluation share one tiny interface: roman word (+ context) -> ranked candidates."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Protocol


class System(Protocol):
    name: str

    def suggest(self, roman: str, context: Sequence[str] = (), k: int = 5) -> list[str]:
        """Return up to k Bengali candidates, best first. May return [] for unknown input."""
        ...


class AvroSystem:
    """Rule-based Avro Phonetic (via the MIT-licensed avro.py port). One candidate, no ranking.

    This is what Avro Keyboard, OpenBangla and Windows' Bangla Phonetic users get today: the rules
    are exact, so loose spellings ("amr", "korci") produce literal garbage.
    """

    name = "avro"

    def __init__(self, remap_words: bool = True) -> None:
        import avro

        self._parse = avro.parse
        self._remap = remap_words

    def suggest(self, roman: str, context: Sequence[str] = (), k: int = 5) -> list[str]:
        out = self._parse(roman, remap_words=self._remap)
        return [out] if out else []


def load_system(name: str, **kwargs) -> System:
    if name == "avro":
        return AvroSystem(**kwargs)
    if name in ("indicxlit", "indicxlit+rerank"):
        from likhi.engine.xlit_np import IndicXlitNumpySystem

        return IndicXlitNumpySystem(rescore=name.endswith("+rerank"), **kwargs)
    if name in ("indicxlit-ct2", "indicxlit-ct2+rerank"):
        from likhi.engine.xlit_ct2 import IndicXlitSystem

        return IndicXlitSystem(rescore=name.endswith("+rerank"), **kwargs)
    if name == "likhi":
        from likhi.engine.core import LikhiSystem

        return LikhiSystem(**kwargs)
    raise KeyError(f"unknown system: {name}")
