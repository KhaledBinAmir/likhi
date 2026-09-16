# Likhi on Windows via PIME

This is the text service itself: the files that PIME loads to turn keystrokes into Bangla. The
installer in `installer/` ships them; the manual steps below are for development.

## What is here

- `likhi/ime.json` — PIME input-method manifest (registers "Likhi (Bangla phonetic)" under
  Bengali (Bangladesh), GUID `{9B4E7C21-3D5A-4F86-A2E1-6C0D8B7F5A13}`).
- `likhi/likhi_ime.py` — the PIME text service. Python 3.8 compatible; talks to `likhi-server`.
- `likhi/config.json` — port, toggle key (default F12), digits and punctuation options.
- `likhi/icon.ico` — the Likhi mark, committed. Regenerate with `make_icon.ps1`.
- `make_icon.ps1` — draws the icon. PowerShell rather than Python because Bengali needs complex
  script shaping and Windows' own text stack does it correctly.

## PIME must be installed at `C:\Program Files (x86)\PIME`

Not a preference. `PIMETextService.dll` builds that path internally and enumerates
`<that path>\python\input_methods\*\ime.json` when `regsvr32` calls `DllRegisterServer`. It
ignores its own location on disk and it ignores `HKLM\SOFTWARE\PIME`. Install it anywhere else
and registration quietly succeeds while writing no language profile, so the keyboard never
appears in the picker and nothing reports an error.

This cost several days: it only showed up on machines that had never installed PIME by hand.
A related trap in the same area — a UTF-8 BOM in `ime.json` makes PIME skip the input method
without complaint.

## Install (needs an administrator prompt twice)

1. Install PIME 1.3.0 (signed by its author) — the installer is already downloaded to the
   session scratchpad, or get it from https://github.com/EasyIME/PIME/releases/tag/v1.3.0-stable.
   Accept the UAC prompt. It installs to `C:\Program Files (x86)\PIME`.
2. Copy this folder's `likhi` directory to
   `C:\Program Files (x86)\PIME\python\input_methods\likhi`.
3. Re-register the text service so PIME picks up the new `ime.json` (admin PowerShell):

   ```
   regsvr32 "C:\Program Files (x86)\PIME\x64\PIMETextService.dll"
   regsvr32 "C:\Program Files (x86)\PIME\x86\PIMETextService.dll"
   ```

4. Settings → Time & language → Language & region → Add a language → **Bengali (Bangladesh)**
   (no language pack needed) → Options → Add a keyboard → **Likhi (Bangla phonetic)**.
5. Start the engine in a normal terminal (keep it running):

   ```
   cd C:\Users\khaled\src\likhi
   .\.venv\Scripts\likhi-server.exe
   ```

6. Win+Space to switch to Likhi, open Notepad, type `amr nam khaled`, press Space after each word.

If PIME's launcher is not running (tray icon), start `C:\Program Files (x86)\PIME\PIMELauncher.exe`.
For logs, run `PIMELauncher.exe /console`.

## What we learn from the test

- Does the TSF DLL load and show candidates in Notepad, Chrome, VS Code, Windows Terminal?
- Is the round trip (key → PIME → Python 3.8 → socket → engine → back) imperceptible?
- Do the known PIME issues on Windows 11 24H2+ (not loading after reboot, Office output) appear?

Outcomes feed the Stage 3 decision: keep PIME, or move to a thin Rust/C++ TSF shell that
speaks the same socket protocol.
