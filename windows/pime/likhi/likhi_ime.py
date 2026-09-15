"""Likhi text service for PIME (runs inside PIME's own Python, which may be as old as 3.8).

This module is deliberately thin: it turns key events into a roman buffer, asks the Likhi engine
server (likhi-server, Python 3.12) for candidates over a local socket, and drives the TSF
composition and candidate window through PIME's TextService API. Keep it 3.8-compatible.

Behaviour (see docs/PLAN.md, UX section):
  letters        -> appended to the composition (shown as typed Latin, underlined)
  Space / Enter  -> commit the highlighted candidate (Space also types a space)
  1..9, 0        -> commit that candidate
  Up/Down/Left/Right -> move the highlight
  Backspace      -> delete one letter (or cancel when empty)
  Esc            -> cancel composition
  punctuation    -> commit the highlighted candidate, then the punctuation ('.' -> '।' in Bangla mode)
  toggle key     -> switch between Bangla and English passthrough (default F12)
"""

import json
import os
import socket

from keycodes import (  # provided by PIME
    VK_BACK,
    VK_CONTROL,
    VK_DOWN,
    VK_ESCAPE,
    VK_F1,
    VK_LEFT,
    VK_MENU,
    VK_RETURN,
    VK_RIGHT,
    VK_SPACE,
    VK_UP,
)
from textService import TextService  # provided by PIME

HERE = os.path.dirname(os.path.abspath(__file__))
ID_TOGGLE = 1
TOGGLE_GUID = "{5E2A9C47-1B3D-4F60-8A7E-9D2C4B6F1E35}"
BANGLA_DIGITS = "০১২৩৪৫৬৭৮৯"
PUNCT_IN_BANGLA = {".": "।"}  # danda


def _load_config():
    cfg = {
        "server_host": "127.0.0.1",
        "server_port": 47123,
        "server_timeout_ms": 250,
        "candidates": 5,
        "toggle_key": "F12",
        "bangla_digits": True,
        "danda_for_period": True,
        "space_commits": True,
        "enter_commits": True,
        "font_name": "Nirmala UI",
        "font_size": 16,
    }
    try:
        with open(os.path.join(HERE, "config.json"), encoding="utf-8") as f:
            cfg.update(json.load(f))
    except Exception:
        pass
    return cfg


def _toggle_vk(name):
    name = (name or "F12").upper()
    if name.startswith("F") and name[1:].isdigit():
        return VK_F1 + int(name[1:]) - 1
    return VK_F1 + 11  # F12


class EngineClient:
    """Tiny JSON-lines client with lazy reconnect; never raises into the key handler."""

    def __init__(self, host, port, timeout_ms):
        self.addr = (host, port)
        self.timeout = timeout_ms / 1000.0
        self.sock = None
        self.rfile = None

    def _connect(self):
        s = socket.create_connection(self.addr, timeout=self.timeout)
        s.settimeout(self.timeout)
        self.sock = s
        self.rfile = s.makefile("rb")

    def request(self, obj):
        for _attempt in range(2):
            try:
                if self.sock is None:
                    self._connect()
                self.sock.sendall((json.dumps(obj, ensure_ascii=False) + "\n").encode("utf-8"))
                line = self.rfile.readline()
                if not line:
                    raise OSError("server closed")
                return json.loads(line.decode("utf-8"))
            except Exception:
                self.close()
        return None

    def close(self):
        try:
            if self.sock is not None:
                self.sock.close()
        except Exception:
            pass
        self.sock = None
        self.rfile = None


class LikhiTextService(TextService):
    def __init__(self, client):
        TextService.__init__(self, client)
        self.cfg = _load_config()
        self.engine = EngineClient(
            self.cfg["server_host"],
            int(self.cfg["server_port"]),
            int(self.cfg["server_timeout_ms"]),
        )
        self.buf = ""
        self.cands = []
        self.cursor = 0
        self.bangla = True

    # ------------------------------------------------------------------ lifecycle

    def onActivate(self):
        TextService.onActivate(self)
        self.customizeUI(
            candFontName=self.cfg["font_name"],
            candFontSize=int(self.cfg["font_size"]),
            candPerRow=int(self.cfg["candidates"]) + 1,
            candUseCursor=True,
        )
        self.setSelKeys("1234567890")
        self.addButton(
            "likhi-mode",
            icon=os.path.join(HERE, "icon.ico"),
            tooltip="Likhi: Bangla / English ({})".format(self.cfg["toggle_key"]),
            commandId=ID_TOGGLE,
        )
        try:
            self.addPreservedKey(_toggle_vk(self.cfg["toggle_key"]), 0, TOGGLE_GUID)
        except Exception:
            pass

    def onDeactivate(self):
        self._reset(commit=False)
        try:
            self.removePreservedKey(TOGGLE_GUID)
        except Exception:
            pass
        self.engine.close()
        TextService.onDeactivate(self)

    def onPreservedKey(self, guid):
        if guid.lower() == TOGGLE_GUID.lower():
            self._toggle()
            return True
        return False

    def onCommand(self, commandId, commandType):
        if commandId == ID_TOGGLE:
            self._toggle()

    def onCompositionTerminated(self, forced):
        # The app took focus away or ended the composition: drop our state, keep whatever the app kept.
        self.buf = ""
        self.cands = []
        self.cursor = 0

    # ------------------------------------------------------------------ keys

    def filterKeyDown(self, keyEvent):
        if not self.bangla:
            return False
        if self.buf:
            return True  # while composing we look at every key
        if keyEvent.isKeyDown(VK_CONTROL) or keyEvent.isKeyDown(VK_MENU):
            return False
        if keyEvent.isChar():
            ch = chr(keyEvent.charCode)
            if ch.isascii() and ch.isalpha():
                return True
            if ch.isdigit() and self.cfg.get("bangla_digits", True):
                return True
            if ch == "." and self.cfg.get("danda_for_period", True):
                return True
        return False

    def onKeyDown(self, keyEvent):
        kc = keyEvent.keyCode
        ch = chr(keyEvent.charCode) if keyEvent.isChar() else ""

        if not self.buf:
            if ch and ch.isascii() and ch.isalpha():
                self.buf = ch
                self._refresh()
                return True
            if ch and ch.isdigit():
                self.setCommitString(BANGLA_DIGITS[int(ch)])
                return True
            if ch == ".":
                self.setCommitString(PUNCT_IN_BANGLA["."])
                return True
            return False

        if kc == VK_BACK:
            self.buf = self.buf[:-1]
            self._refresh()
            return True
        if kc == VK_ESCAPE:
            self._reset(commit=False)
            return True
        if kc == VK_SPACE:
            self._commit(self._current(), " " if self.cfg.get("space_commits", True) else "")
            return True
        if kc == VK_RETURN:
            self._commit(self._current(), "")
            return True
        if kc in (VK_DOWN, VK_RIGHT):
            self._move(1)
            return True
        if kc in (VK_UP, VK_LEFT):
            self._move(-1)
            return True
        if ch and ch.isdigit():
            idx = 9 if ch == "0" else int(ch) - 1
            if idx < len(self.cands):
                self._commit(self.cands[idx], "")
            return True
        if ch and ch.isascii() and ch.isalpha():
            self.buf += ch
            self._refresh()
            return True
        if ch and ch.isprintable():
            trailing = ch
            if ch == "." and self.cfg.get("danda_for_period", True):
                trailing = PUNCT_IN_BANGLA["."]
            self._commit(self._current(), trailing)
            return True
        return True  # swallow anything else while composing

    def filterKeyUp(self, keyEvent):
        return False

    # ------------------------------------------------------------------ helpers

    def _toggle(self):
        self._reset(commit=True)
        self.bangla = not self.bangla
        self.changeButton(
            "likhi-mode", tooltip="Likhi: %s" % ("Bangla" if self.bangla else "English")
        )
        self.showMessage("Likhi: %s" % ("বাংলা" if self.bangla else "English"), 1)

    def _current(self):
        if self.cands and 0 <= self.cursor < len(self.cands):
            return self.cands[self.cursor]
        return self.buf

    def _move(self, delta):
        if not self.cands:
            return
        self.cursor = (self.cursor + delta) % len(self.cands)
        self.setCandidateCursor(self.cursor)

    def _refresh(self):
        if not self.buf:
            self._reset(commit=False)
            return
        self.setCompositionString(self.buf)
        self.setCompositionCursor(len(self.buf))
        resp = self.engine.request(
            {"op": "suggest", "roman": self.buf, "k": int(self.cfg["candidates"])}
        )
        cands = list(resp.get("candidates", [])) if resp and resp.get("ok") else []
        if self.buf not in cands:
            cands.append(self.buf)  # the raw Latin is always reachable
        self.cands = cands
        self.cursor = 0
        self.setCandidateList(self.cands)
        self.setCandidateCursor(0)
        self.setShowCandidates(True)

    def _commit(self, text, trailing):
        roman = self.buf
        self.setCommitString(text + trailing)
        self.setCompositionString("")
        self.setShowCandidates(False)
        if text != roman:
            self.engine.request({"op": "learn", "roman": roman, "chosen": text})
        self.buf = ""
        self.cands = []
        self.cursor = 0

    def _reset(self, commit):
        if self.buf and commit:
            self.setCommitString(self._current())
        self.setCompositionString("")
        self.setShowCandidates(False)
        self.buf = ""
        self.cands = []
        self.cursor = 0
