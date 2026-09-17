r"""A named pipe the keyboard can reach from inside a Store application.

An application from the Microsoft Store -- Unigram, WhatsApp, Mail -- runs in an AppContainer, and
an AppContainer cannot open a loopback socket. The text service runs *inside* that application, so
its connection to 127.0.0.1 fails and the keyboard falls back to typing Latin: the pilot saw exactly
that in Unigram. A named pipe has no such restriction, provided two things are right, and both are
easy to get wrong in ways that fail silently:

* the pipe's DACL must grant ALL APPLICATION PACKAGES. An AppContainer token carries restricted
  SIDs, so an ACE naming the user is not enough on its own;
* the pipe must carry a *low* mandatory integrity label. AppContainers run at low integrity, and
  Windows forbids a low-integrity process from writing to a medium-integrity object whatever the
  DACL says. Without the label the connection is refused after passing every other check.

Both live in one SDDL string below. This is the mechanism PIME used, and the thing we gave up by
moving to a TCP socket.

The pipe name carries the Windows session id, so two people signed in to the same machine each get
their own engine and their own personal dictionary rather than fighting over one pipe.

ctypes rather than pywin32: the engine ships as an embedded interpreter with three wheels in it, and
one more dependency for four API calls is not worth the size.
"""

from __future__ import annotations

import ctypes
import threading
from ctypes import wintypes
from typing import Callable

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)

PIPE_ACCESS_DUPLEX = 0x00000003
PIPE_TYPE_BYTE = 0x00000000
PIPE_READMODE_BYTE = 0x00000000
PIPE_WAIT = 0x00000000
PIPE_UNLIMITED_INSTANCES = 255
INVALID_HANDLE_VALUE = wintypes.HANDLE(-1).value
ERROR_PIPE_CONNECTED = 535
ERROR_BROKEN_PIPE = 109
ERROR_NO_DATA = 232
BUFFER_BYTES = 64 * 1024

# SYSTEM and administrators get everything; the owner (whoever started the engine) gets everything;
# application packages get read and write, which is the point of the exercise. The SACL sets a low
# mandatory label with no write-up restriction, so an AppContainer at low integrity may talk to a
# pipe created at medium.
#
# Two package SIDs, not one. AC is ALL APPLICATION PACKAGES, which covers an ordinary Store
# application such as Unigram. S-1-15-2-2 is ALL RESTRICTED APPLICATION PACKAGES, and a Less
# Privileged AppContainer -- Edge, and a growing number of Store applications -- does not carry AC
# in its token at all, so an ACE naming only AC would let it fail exactly the way Unigram did. There
# is no portable SDDL alias for it, hence the literal SID.
ALL_APPLICATION_PACKAGES = "AC"
ALL_RESTRICTED_APPLICATION_PACKAGES = "S-1-15-2-2"
SDDL = (
    "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)"
    f"(A;;GRGW;;;{ALL_APPLICATION_PACKAGES})"
    f"(A;;GRGW;;;{ALL_RESTRICTED_APPLICATION_PACKAGES})"
    "S:(ML;;NW;;;LW)"
)

SDDL_REVISION_1 = 1


class SECURITY_ATTRIBUTES(ctypes.Structure):
    _fields_ = [
        ("nLength", wintypes.DWORD),
        ("lpSecurityDescriptor", ctypes.c_void_p),
        ("bInheritHandle", wintypes.BOOL),
    ]


def session_id() -> int:
    """This process's Windows session. Part of the pipe name, so each signed-in user has their own."""
    pid = kernel32.GetCurrentProcessId()
    out = wintypes.DWORD()
    if not kernel32.ProcessIdToSessionId(pid, ctypes.byref(out)):
        return 0
    return int(out.value)


def pipe_name(session: int | None = None) -> str:
    return rf"\\.\pipe\likhi-engine-s{session_id() if session is None else session}"


def _security_attributes() -> SECURITY_ATTRIBUTES:
    descriptor = ctypes.c_void_p()
    ok = advapi32.ConvertStringSecurityDescriptorToSecurityDescriptorW(
        ctypes.c_wchar_p(SDDL),
        wintypes.DWORD(SDDL_REVISION_1),
        ctypes.byref(descriptor),
        None,
    )
    if not ok:
        raise ctypes.WinError(ctypes.get_last_error())
    sa = SECURITY_ATTRIBUTES()
    sa.nLength = ctypes.sizeof(SECURITY_ATTRIBUTES)
    sa.lpSecurityDescriptor = descriptor
    sa.bInheritHandle = False
    # The descriptor is deliberately not freed: it is referenced for the life of every pipe instance
    # created from it, and the engine holds those for the life of the process.
    return sa


def _create_instance(name: str, sa: SECURITY_ATTRIBUTES) -> int:
    handle = kernel32.CreateNamedPipeW(
        ctypes.c_wchar_p(name),
        wintypes.DWORD(PIPE_ACCESS_DUPLEX),
        wintypes.DWORD(PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT),
        wintypes.DWORD(PIPE_UNLIMITED_INSTANCES),
        wintypes.DWORD(BUFFER_BYTES),
        wintypes.DWORD(BUFFER_BYTES),
        wintypes.DWORD(0),
        ctypes.byref(sa),
    )
    if handle == INVALID_HANDLE_VALUE:
        raise ctypes.WinError(ctypes.get_last_error())
    return handle


def _read(handle: int, size: int = BUFFER_BYTES) -> bytes:
    buf = ctypes.create_string_buffer(size)
    read = wintypes.DWORD()
    ok = kernel32.ReadFile(
        wintypes.HANDLE(handle), buf, wintypes.DWORD(size), ctypes.byref(read), None
    )
    if not ok or read.value == 0:
        return b""
    return buf.raw[: read.value]


def _write(handle: int, data: bytes) -> bool:
    written = wintypes.DWORD()
    ok = kernel32.WriteFile(
        wintypes.HANDLE(handle),
        data,
        wintypes.DWORD(len(data)),
        ctypes.byref(written),
        None,
    )
    return bool(ok)


def _serve_client(handle: int, handle_line: Callable[[bytes], bytes]) -> None:
    """One connection, many request lines, until the client goes away."""
    pending = b""
    try:
        while True:
            chunk = _read(handle)
            if not chunk:
                return
            pending += chunk
            while b"\n" in pending:
                line, pending = pending.split(b"\n", 1)
                if not line.strip():
                    continue
                if not _write(handle, handle_line(line) + b"\n"):
                    return
    finally:
        kernel32.FlushFileBuffers(wintypes.HANDLE(handle))
        kernel32.DisconnectNamedPipe(wintypes.HANDLE(handle))
        kernel32.CloseHandle(wintypes.HANDLE(handle))


def serve(handle_line: Callable[[bytes], bytes], stop: threading.Event) -> str:
    """Accept connections until `stop` is set. Returns the pipe name it is listening on.

    Runs its accept loop on a thread of its own; each connection gets another. The engine answers
    within a deadline and connections are few -- one per application with the keyboard active --
    so a thread each is simpler than overlapped I/O and costs nothing that matters.
    """
    name = pipe_name()
    sa = _security_attributes()

    def accept_loop() -> None:
        while not stop.is_set():
            try:
                handle = _create_instance(name, sa)
            except OSError:
                stop.wait(1.0)
                continue
            connected = kernel32.ConnectNamedPipe(wintypes.HANDLE(handle), None)
            if not connected and ctypes.get_last_error() != ERROR_PIPE_CONNECTED:
                kernel32.CloseHandle(wintypes.HANDLE(handle))
                continue
            if stop.is_set():
                kernel32.CloseHandle(wintypes.HANDLE(handle))
                return
            threading.Thread(
                target=_serve_client, args=(handle, handle_line), daemon=True
            ).start()

    threading.Thread(target=accept_loop, daemon=True).start()
    return name
