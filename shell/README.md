# Likhi text service, in Rust

A Windows Text Services Framework keyboard that talks to the Likhi engine. It replaces the
PIME-based shell (`windows/pime/`) — the C++ DLL, the launcher, and the Python 3.8 backend — with
one DLL per architecture and no other moving parts.

## Why

Every significant bug in the pilot came from the PIME layer, not the engine:

- registration read input methods from a hardcoded `%ProgramFiles(x86)%\PIME` path;
- the candidate window paints from `GetSysColor` and cannot be restyled;
- the backend is pinned to Python 3.8;
- Explorer crashed inside `PIMETextService.dll` on a pilot machine;
- we shipped a prebuilt LGPL DLL we could not modify.

The engine stayed where it was: 0.8 ms per keystroke in Python, reached over the same JSON-lines
socket on 127.0.0.1:47123. The protocol did not change, so the engine did not either.

Rust rather than C++ because this DLL is loaded into every application that has keyboard focus.
A memory-safety bug here takes down Explorer, the browser and everything else at once — we watched
exactly that happen with a text service DLL during the pilot. `windows-rs` is Microsoft's own
binding and generates the COM vtables, which is most of what makes hand-written TSF code long.

## Layout

| file | what |
|---|---|
| `src/lib.rs` | DLL entry points: `DllMain`, `DllGetClassObject`, `DllCanUnloadNow`, `DllRegisterServer`, `DllUnregisterServer`; the class factory |
| `src/registry.rs` | COM class, language profile (`ITfInputProcessorProfileMgr::RegisterProfile`) and categories |
| `src/service.rs` | the text service: activation, key sink, composition, commit |
| `src/edit.rs` | edit sessions — the only way TSF lets a document be changed |
| `src/guids.rs` | CLSID, profile GUID, language id, the US substitute layout |
| `src/log.rs` | append-only log at `%LOCALAPPDATA%\Likhi\shell.log`, plus `OutputDebugString` |
| `exports.def` | pins the five export names on both architectures |

## Milestones

1. **Register, appear in Win+Space, compose, commit** — the whole TSF chain, with a placeholder
   conversion (typed Latin committed in upper case). *Done: builds, registers, profile appears.*
2. Engine client over the socket; candidate window drawn with DirectWrite (proper Bengali shaping,
   dark mode); display attribute for the composition underline; Space/Enter/1–5/arrows exactly as
   the PIME shell behaves today.
3. Replace PIME in the installer. Same product name, same profile behaviour, no launcher, no PIME
   directory.

## The substitute layout

`RegisterProfile` is passed `0x04090409`, US English, as the layout this service sits on. That is
the structural fix for the pilot's worst bug: Windows attaches Bengali INSCRIPT to `bn-BD`, and a
text service that reads the layout's characters then sees Bangla letters instead of Latin ones.
The PIME shell had to work around it by reading virtual key codes; here it cannot arise.

## Building

```
python scripts/build_shell.py            # release, both targets -> dist/shell/{x64,x86}/
python scripts/build_shell.py --debug    # faster, with symbols
```

Needs rustup with the `x86_64-pc-windows-msvc` and `i686-pc-windows-msvc` targets, and the Visual
Studio Build Tools C++ workload (linker and Windows SDK). `rustc` finds the linker itself.

## Developing against a live machine

The DLL is loaded into every application that switches to the keyboard, and a loaded DLL cannot be
overwritten. Deleting one that is mapped crashes the process that mapped it. So the loop is:

1. build;
2. close the applications you tested in (Notepad), so the DLL unloads;
3. copy to `%LOCALAPPDATA%\Likhi\shell\x64\LikhiTextService.dll` and re-register (`regsvr32`, elevated);
4. switch to the keyboard in Notepad, type, read `%LOCALAPPDATA%\Likhi\shell.log`.

It registers under its own CLSID and profile, so it sits beside the PIME shell without touching it;
during development it is named "Likhi (new shell, preview)" in the picker for that reason.

Unregister with `regsvr32 /u` on the same path.
