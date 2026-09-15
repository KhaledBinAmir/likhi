"""Personal model: what this user picked for what they typed.

Two signals, both stored in a small SQLite file that the user owns and can export or delete:

* selections: (normalized roman, chosen word) -> count, last used. "If I keep picking the second
  suggestion for a word, it becomes first." Also how English passthrough is learned: choosing the
  raw Latin for "ok" a few times makes Latin the top candidate for "ok".
* words: chosen word -> count. A personal unigram that lifts the user's own vocabulary (names,
  slang, workplace words) everywhere, and that keeps words the public lexicon never saw.

Scores are log-count style with recency decay, designed to be added to the ranker's log-linear
score. Nothing here ever leaves the machine.
"""

from __future__ import annotations

import math
import os
import sqlite3
import time
from pathlib import Path

from likhi.engine.textnorm import canonical, normalize_roman

DEFAULT_DB = Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "Likhi" / "personal.sqlite"
HALF_LIFE_DAYS = 90.0


class PersonalStore:
    def __init__(
        self, path: Path | str | None = None, *, half_life_days: float = HALF_LIFE_DAYS
    ) -> None:
        self.path = Path(path) if path is not None else DEFAULT_DB
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.db = sqlite3.connect(str(self.path), check_same_thread=False)
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute(
            "CREATE TABLE IF NOT EXISTS selections (roman TEXT, word TEXT, count REAL, last REAL, PRIMARY KEY (roman, word))"
        )
        self.db.execute(
            "CREATE TABLE IF NOT EXISTS words (word TEXT PRIMARY KEY, count REAL, last REAL)"
        )
        self.db.commit()
        self.decay = math.log(2) / (half_life_days * 86400.0)
        # small in-memory caches, invalidated on learn()
        self._sel_cache: dict[str, dict[str, float]] = {}
        self._word_cache: dict[str, float] = {}

    # ------------------------------------------------------------------ writes

    def learn(
        self, roman: str, chosen: str, context: tuple[str, ...] = (), *, when: float | None = None
    ) -> None:
        r = normalize_roman(roman)
        w = chosen if chosen == roman else canonical(chosen)
        if not r or not w:
            return
        now = when if when is not None else time.time()
        cur = self.db.execute(
            "SELECT count, last FROM selections WHERE roman=? AND word=?", (r, w)
        ).fetchone()
        count = self._decayed(cur[0], cur[1], now) + 1.0 if cur else 1.0
        self.db.execute(
            "INSERT OR REPLACE INTO selections (roman, word, count, last) VALUES (?,?,?,?)",
            (r, w, count, now),
        )
        cur = self.db.execute("SELECT count, last FROM words WHERE word=?", (w,)).fetchone()
        wcount = self._decayed(cur[0], cur[1], now) + 1.0 if cur else 1.0
        self.db.execute(
            "INSERT OR REPLACE INTO words (word, count, last) VALUES (?,?,?)", (w, wcount, now)
        )
        self.db.commit()
        self._sel_cache.pop(r, None)
        self._word_cache.pop(w, None)

    def forget(self, roman: str | None = None, word: str | None = None) -> None:
        if roman is not None and word is not None:
            self.db.execute(
                "DELETE FROM selections WHERE roman=? AND word=?", (normalize_roman(roman), word)
            )
        elif word is not None:
            self.db.execute("DELETE FROM selections WHERE word=?", (word,))
            self.db.execute("DELETE FROM words WHERE word=?", (word,))
        else:
            self.db.execute("DELETE FROM selections")
            self.db.execute("DELETE FROM words")
        self.db.commit()
        self._sel_cache.clear()
        self._word_cache.clear()

    # ------------------------------------------------------------------ reads

    def _decayed(self, count: float, last: float, now: float) -> float:
        return count * math.exp(-self.decay * max(0.0, now - last))

    def selections(self, roman: str, *, now: float | None = None) -> dict[str, float]:
        """Decayed counts of words chosen for this exact roman string."""
        r = normalize_roman(roman)
        if r in self._sel_cache:
            return self._sel_cache[r]
        now = now if now is not None else time.time()
        rows = self.db.execute(
            "SELECT word, count, last FROM selections WHERE roman=?", (r,)
        ).fetchall()
        out = {w: self._decayed(c, last, now) for w, c, last in rows}
        self._sel_cache[r] = out
        return out

    def word_count(self, word: str, *, now: float | None = None) -> float:
        if word in self._word_cache:
            return self._word_cache[word]
        now = now if now is not None else time.time()
        row = self.db.execute("SELECT count, last FROM words WHERE word=?", (word,)).fetchone()
        v = self._decayed(row[0], row[1], now) if row else 0.0
        self._word_cache[word] = v
        return v

    def known_words(self) -> list[str]:
        return [w for (w,) in self.db.execute("SELECT word FROM words WHERE count >= 1")]

    def export_jsonl(self, path: Path | str) -> int:
        import json

        n = 0
        with open(path, "w", encoding="utf-8") as f:
            for roman, word, count, last in self.db.execute(
                "SELECT roman, word, count, last FROM selections"
            ):
                f.write(
                    json.dumps(
                        {"roman": roman, "word": word, "count": count, "last": last},
                        ensure_ascii=False,
                    )
                    + "\n"
                )
                n += 1
        return n

    def close(self) -> None:
        self.db.close()
