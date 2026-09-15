# Custom system-wide IME on Windows 10/11 (verified 2026-09-15)

## 1. Text Services Framework (TSF)

- Minimum interfaces: `ITfTextInputProcessor` (+Ex), `ITfEditSession`; SampleIME also implements `ITfThreadMgrEventSink, ITfTextEditSink, ITfKeyEventSink, ITfCompositionSink, ITfDisplayAttributeProvider, ITfActiveLanguageProfileNotifySink, ITfThreadFocusSink, ITfFunctionProvider, ITfFnGetPreferredTouchKeyboardLayout`.
  - https://github.com/microsoft/Windows-classic-samples/blob/main/Samples/IME/cpp/SampleIME/SampleIME.h (C++, Win 8.1 era, 2 commits 2015/2016)
- Registration = admin/HKLM: COM in-proc server + `ITfInputProcessorProfileMgr::RegisterProfile` + `ITfCategoryMgr::RegisterCategory` (`GUID_TFCAT_TIP_KEYBOARD`, `GUID_TFCAT_TIPCAP_UIELEMENTENABLED`, `GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT`). HKCU registration does not produce functional TIPs (Praetorian 2025).
  - https://learn.microsoft.com/en-us/windows/win32/tsf/text-service-registration ; https://learn.microsoft.com/en-us/windows/apps/design/input/input-method-editor-requirements
- In-process: TSF loads the IME DLL into every app; must ship x86/x64/ARM64; must not declare its own DPI awareness. PIME registers `x86\`, `x64\`, `arm64\PIMETextService.dll`; Mozc builds tip32/64/64arm/64x; Keyman ships kmtip.dll/kmtip64.dll/kmtiparm64x.dll; Weasel uses an ARM64X forwarder.
- UI-less mode: `ITF_AE_UIELEMENTENABLEDONLY`, `ITfUIElementMgr::BeginUIElement`, `ITfCandidateListUIElement`. https://learn.microsoft.com/en-us/windows/win32/tsf/uiless-mode-overview
- App support: Chromium unconditionally creates `InputMethodWinTSF` (Chrome, Electron: VS Code, Slack). Windows Terminal + conhost share a Win32 TSF implementation since PR #17067 (merged 2024-04-18). Office is TSF-enabled. WhatsApp Desktop moved to a WebView2/Chromium wrapper in July 2025. Electron apps lose IME after RDP disconnect (electron#41393, not planned). RDP needs unicode keyboard mode for IMEs.
  - https://chromium.googlesource.com/chromium/src/+/main/ui/base/ime/init/input_method_factory.cc ; https://github.com/microsoft/terminal/pull/17067 ; https://github.com/microsoft/vscode/wiki/IME-Test

## 2. PIME (https://github.com/EasyIME/PIME)

- Status: 1,473 stars, 352 open issues, LGPL-2.1 (+Apache/PSF parts); last push 2026-07-25 (backend race fix, SignPath signing, VS 2026 CI). Last release v1.3.0-stable 2023-01-20 (bundled Python 3.8.10). Issue #888 (2026-08-18, open): PIMETextService.dll crashes Explorer/Task Manager/conhost/SearchHost when launcher RPC fails.
- Architecture: libIME2 (C++ TSF wrapper, LGPL-2.1, last commit 2026-05-17) → `PIMETextService.dll` → named pipe `\\.\pipe\<user>\PIME\Launcher` → PIMELauncher (Rust/Tokio, i686) spawns/monitors Python, Node, Go backends; line-based UTF-8 JSON protocol. Pipe ACL grants ALL APPLICATION PACKAGES + Low IL label.
  - https://github.com/EasyIME/PIME/blob/master/PIMELauncher/README.md ; README_SPEC.md ; PIMELauncher/src/acl.rs ; https://github.com/EasyIME/libIME2
- Python backend API (`python/textService.py`): `onActivate/onDeactivate`, `filterKeyDown/onKeyDown`, `filterKeyUp/onKeyUp`, `onPreservedKey`, `onCommand`, `onMenu`, `onCompartmentChanged`, `onKeyboardStatusChanged`, `onCompositionTerminated`; output: `setCompositionString`, `setCompositionCursor`, `setCommitString`, `setCandidateList`, `setCandidateCursor`, `setShowCandidates`, `setSelKeys`, `customizeUI(candFontName, candFontSize, candPerRow, candUseCursor)`, `addButton/removeButton/changeButton`, `addPreservedKey`, `showMessage`.
- Candidate window: libIME2 in-process GDI popup (`ExtTextOut`, WS_POPUP|WS_EX_TOOLWINDOW|WS_EX_TOPMOST, no DPI code); customization limited (#798 open for colors).
- Known issues: UWP/SearchUI input fails (#469, open since 2018), run-as-other-user (#646), Win11 24H2 "not loaded after reboot"/"IME disabled" (#855/#856), Office no output (#840/#859). ARM64 runtime status UNVERIFIED.
- Built on PIME: chewing family, rime (Python), emojime (Node), McBopomofo for Windows, Yime, fork PRIME.

## 3. Other TSF front-ends

- Weasel (RIME): C++/WTL, GPL-3.0, last release 0.17.4 (2025-06-04), last commit 2026-08-18. Out-of-process: WeaselServer owns librime + UI; TSF DLL talks over named pipe + shared memory. librime 1.17.0 (2026-06-06, BSD-3): YAML schemas, `table_translator`/`script_translator`, Lua translators/filters via librime-lua. Spelling algebra (`xlit/xform/derive/abbrev/fuzz/erase`) = regex over input codes, usable for Latin→Bengali fuzz. No Bengali schema found.
  - https://github.com/rime/weasel ; https://github.com/rime/librime ; https://github.com/rime/home/wiki/SpellingAlgebra
- kime: no Windows frontend. Rust TSF: koyubi (MIT, SKK, x86_64, 2026-03), ime-rs (SampleIME port, x64+arm64, windows-rs 0.60, 2025-03), hufu-ime-rust (GPL-3, Direct2D+Acrylic candidates, named-pipe server, last commit 2026-09-13). khiin is C++17 (MIT, 2023).
  - https://github.com/saschanaz/ime-rs ; https://github.com/LeafHW/hufu-ime-rust ; https://github.com/barewalker/koyubi
- Keyman: MIT; Windows 18.0.249 (2026-03-27); keyman32/64/arm64.dll hook layer + kmtip TSF; predictive text NOT on desktop (#11915 closed not planned 2024-07-02). Bengali keyboards exist (sil_bengali_phonetic, bangla_probhat, ...).
- Mozc: BSD-3, TIP + separate `mozc_renderer.exe` (GDI + Direct2D, PerMonitorV2).
- fcitx5-windows: "currently not working" (2026-06-09).
- OpenBangla Keyboard: PR #455 "Windows port" merged 2026-07-12 into `develop`: Windows TSF IME DLL, Qt candidate window, 32/64-bit, NSIS installer needing admin; no Windows release yet (latest 2.0.0, 2020-10-01); open Win11 bug #470 (preview window clipped near taskbar). riti: Rust, MPL-2.0, C FFI, last commit 2026-07-04.
  - https://github.com/OpenBangla/OpenBangla-Keyboard/pull/455 ; https://github.com/OpenBangla/riti

## 4. Avro Keyboard and Windows Bangla Phonetic

- Source: https://github.com/OmicronLab/Avro-Keyboard, Delphi 2010, MPL 1.1/2.0, last push 2012-06-24. Binaries 5.6.0 (2019-08-27).
- Mechanism (verified): `SetWindowsHookEx(WH_KEYBOARD_LL, ...)` swallowing keys; output via `SendInput` with `KEYEVENTF_UNICODE`; corrections via loop of `SendInput(VK_Back)`. Not TSF.
- Dictionary: ~150,000-word auto-correct dictionary (`uAutoCorrect.pas`, `uSimilarSort.pas`, `Levenshtein.pas`). Per-user learning UNVERIFIED.
- Problems: garbled output in browsers on Win11 22H2 (mugli/Avro-Keyboard#24), security-software interference, elevated apps need Avro run as admin (UIPI).
- Windows Bangla Phonetic: Insider build 18272 (2018-10-31), rules "based on ISO 15919", dictionary via Windows Update, added under Bengali (India); Enter commits first suggestion; July 2025 KB5062553 broke phonetic prediction.
  - https://learn.microsoft.com/en-us/globalization/input/bengali-ime

## 5. Non-TSF alternative: WH_KEYBOARD_LL + SendInput

- Hook must finish within `LowLevelHooksTimeout` (max 1000 ms since 1709); on timeout the hook is silently removed (Win7+). Python listeners show system-wide lag (pynput#438).
  - https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc
- `SendInput` subject to UIPI (cannot inject into higher-IL apps; failure unreported). Only UIAccess apps (signed, Program Files) bypass.
- `KEYEVENTF_UNICODE` → VK_PACKET/WM_CHAR; injected events flagged `LLKHF_INJECTED`.
- No composition/underline, no document access; backspace-and-retype artifacts; AV false positives (AutoHotkey FAQ; PIME release notes on signing).
- Users of this approach: Avro (verified), Keyman hook layer (hybrid), Google Input Tools for Windows (removed 2018), Borno (mechanism UNVERIFIED).

## 6. Candidate window UI

- In-process GDI: PIME/libIME2, SampleIME. Out-of-process: Weasel (DirectWrite), Mozc renderer (Direct2D, PerMonitorV2), hufu (Direct2D), OBK Windows (Qt).
- DPI: in-proc TIP inherits host awareness; `SetThreadDpiAwarenessContext` since 1607; separate helper process controls its own manifest.
- Fonts: Nirmala UI is the base UI font since Windows 8 (present on this machine); Vrinda/Shonar Bangla ship in the optional "Bangla Script Supplemental Fonts" package, auto-added when a Bangla keyboard is enabled.

## UNVERIFIED / could not find
- Chromium hang bug 328859185 details; Avro 5.6 source; Avro learning; PIME ARM64 runtime; Google Input Tools and Borno mechanisms; any Rime Bengali schema.
