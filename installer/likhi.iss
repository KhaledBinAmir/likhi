; Likhi installer. Build with:
;   python scripts/build_engine.py
;   python scripts/build_client.py
;   python scripts/build_app.py
;   "%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe" installer\likhi.iss
;
; Produces dist\LikhiSetup-<version>.exe: one per-machine installer that needs administrator
; rights once and leaves the user with a working Bangla keyboard and nothing to configure.

#define AppName "Likhi"
; 0.4.1: the engine no longer puts a console window on the desktop at sign-in. It starts from the
;        Run key, and as a console application Windows gave it a terminal titled with the install
;        path, on every machine, at every boot. It is a windows-subsystem binary now and borrows the
;        parent's console when started by hand, so running it from a terminal still prints. With no
;        console there is nowhere for it to print at sign-in, so it also writes
;        %LOCALAPPDATA%\Likhi\engine.log -- a tester cannot send a log that was never written.
; 0.4.0: suggestions appear in sandboxed applications. A window created by a process inside an
;        AppContainer never reaches the desktop: CreateWindowExW returns a handle, SetWindowPos
;        reports success, and nothing is composed. So in Telegram Likhi typed Bangla, showed no
;        list, and committed whatever it had ranked first. Established by enumerating every window
;        on the machine while typing -- ours was in Code, Notepad and WhatsApp, and never in
;        Telegram. WhatsApp is also a Store application and works, because it runs at full trust,
;        so the line is the sandbox rather than the Store and no application can opt out of it.
;        The engine is an ordinary user process, so it draws the list on the text service's behalf;
;        only the drawing moved, and applications that already worked still draw their own.
;        The text service also starts the engine when nothing answers. A Windows update restarted a
;        machine, the engine did not come back, and the keyboard did nothing everywhere with no
;        indication why -- which reads as the product being broken and produces no useful report.
;        Two waits were moved off the thread applications draw on: starting the engine ran
;        CreateProcess inside a keystroke, where Windows Defender's scan of a newly installed binary
;        costs 102-155 ms against a 30 ms budget, and the candidate window was built on first use
;        while a text service sat blocked on the reply.
; 0.3.0: the engine, the lexicon builder, the evaluation harness and the tuner are Rust. The Python
;        they replaced is gone, after each tool was checked against the one it replaced and produced
;        the same numbers. No interpreter ships: installer 57.8 -> 39.2 MB, start-up 630 -> 31 ms,
;        and unreclaimable memory 236 -> 6 MB, which is what matters on the machines this runs on.
; 0.2.4: a full stop after a space typed a period rather than the dari -- punctuation was handled
;        only inside a composition, and after Space there is none. The suggestion list often failed
;        to appear for the first letter of a word, because GetTextExt on a composition created a
;        moment earlier reports it as not laid out; it falls back to the caret now. The shell
;        under-reported to telemetry: the engine's learn call is also the pilot's counting path, and
;        it was sent only when the committed word differed from the Latin, without the candidate
;        position -- so "first suggestion taken" counted nothing for the first suggestion. Every
;        commit is reported now, with position, whether the word was retyped, and the application.
;        The list refines itself when you pause, but only when the fast answer is not already
;        attested: refining everything measured 73.9 top-1 against 75.7 gated, and on chat words it
;        was a regression. A dead engine no longer costs every keystroke a connection attempt.
; 0.2.3: the setting that pointed Bangla at a chosen font across the whole machine is removed,
;        because it did not work. Windows has no Bangla font to change: an application asks for
;        Segoe UI, which has no Bengali glyphs, and something else supplies them. Choosing that
;        something is font linking, and font linking is a GDI mechanism -- modern applications draw
;        through DirectWrite, which picks a fallback family itself and never reads those keys.
;        Measured, not assumed: with the entries in place and the font installed machine-wide,
;        Bangla under Segoe UI still rendered at Nirmala UI's exact metrics. The only thing that
;        would work is replacing the system font file, which means breaking Windows servicing and
;        the nine other Indic scripts Nirmala UI carries. A switch that does nothing is worse than
;        no switch. The font picker still sets the suggestion list, and says so.
; 0.2.2: nothing chosen in the Likhi window was ever saved. Refresh2 saves and restores the
;        "loading" guard around its work, and during construction the saved value was true, so
;        clearing the guard inside it left the restore to put true straight back: the flag never
;        cleared and every handler returned early. Cleared at the end of the constructor instead.
;        The shipped config also still named Nirmala UI as the candidate font, which overrode the
;        preference chain on every machine; it names Noto Sans Bengali, which we now install.
; 0.2.1: the Likhi window checked the old text service's identity, so it reported the keyboard as
;        missing on a working install; it also wrote a font nobody had chosen, because filling a
;        drop-down raises the same event a click does. Choosing a candidate with an arrow key or a
;        number now settles the list to the full ranking first, so what you pick is what you saw:
;        the list shown while typing is the fast one, and for some words it differs. And Bangla can
;        be pointed at a chosen font across the whole machine, reversibly.
; 0.2.0: our own text service, in Rust, replacing PIME entirely. One DLL per architecture and the
;        engine; no launcher, no second Python interpreter, no shared PIME directory. Every serious
;        bug of the pilot came from that layer rather than from the engine -- registration reading a
;        hardcoded path, a candidate window that could not be restyled, a backend pinned to Python
;        3.8, and Explorer crashing inside PIMETextService.dll on a pilot machine.
;        The candidate window is ours now: Direct2D and DirectWrite, follows the Windows theme,
;        shapes Bengali correctly, and takes a font the person chooses. Four Bangla faces under the
;        SIL Open Font License ship with it and are installed for every application, not just ours.
;        An upgrade unregisters the old service and takes it out of the language list, so nobody is
;        left with two Bangla keyboards -- the same trap the INSCRIPT layout set in 0.1.8.
; 0.1.12: a Start menu entry called simply "Likhi". Installing a keyboard leaves nothing to click,
;        and everyone looks for an app: pilot users searched the Start menu, found nothing, and had
;        no way to tell a working install from a broken one. The window says whether the keyboard
;        and the engine are working, how to switch to Bangla, gives somewhere safe to try typing,
;        and carries the two settings that belong to the person rather than the machine -- start at
;        sign-in, and whether to share usage data. Usage settings are stored per user and merged
;        over the installed config, so turning reporting off does not need an administrator and does
;        not decide for anyone else on a shared machine.
; 0.1.11: the list shown while typing no longer contains words that match nothing you typed. The
;        fast path runs without the transliteration model, and the aligned romanization data has a
;        tail of misaligned pairs; one of those plus a high unigram count was enough to reach the
;        visible list unopposed, so "bangla" offered কোন and হিসেবে and "sonar" offered খনির. That
;        list is selectable, so pressing 4 committed a word the user never typed. Candidates whose
;        entire case is a single attested pair now rank behind anything with phonetic agreement,
;        and only when something is well attested for that spelling. Found from a pilot screenshot:
;        the full path ranks these away, so every offline measurement looked fine.
; 0.1.10: candidate window font down to 14px from 16. Font family, pixel size and how many
;        candidates share a row are the only parts of that window an input method can change --
;        PIME's customizeUI takes candFontName, candFontSize, candPerRow and candUseCursor, and
;        nothing else. Its colours come from GetSysColor inside the text service DLL, so a dark
;        theme, rounded corners or any other restyling needs a different candidate window, not a
;        setting. font_size in config.json is a live knob: edit it and restart the launcher.
; 0.1.9: the text service now reads the physical key rather than the character the keyboard layout
;        underneath it produced. A text service sits on top of a layout, and Windows attaches
;        Bengali INSCRIPT to bn-BD, which maps the letter keys straight onto Bangla letters -- so
;        every key arrived as a non-ASCII character, we declined it, PIME passed it to the
;        application, and the user got raw INSCRIPT. Reported from the pilot as "Likhi types random
;        Bangla" even with Likhi selected in the picker. It worked on the development machine only
;        because its bn-BD layout was substituted with US English. 0.1.8 removed the INSCRIPT
;        keyboard, which hides the symptom; this fixes the cause, so any layout works.
; 0.1.8: remove the decoy keyboard. Adding bn-BD to the language list makes Windows attach that
;        language's default physical layout too, Bengali INSCRIPT (0845:00000445), which then sits
;        next to Likhi in Win+Space. INSCRIPT maps QWERTY keys straight onto Bangla letters, so a
;        pilot user who lands on it types what looks like gibberish and reasonably concludes the
;        keyboard is broken. Two of the first three machines hit exactly that. Bangla now means
;        Likhi; anyone who actually wants INSCRIPT can add it in Settings.
;        Also clears the forced Likhi default that versions up to 0.1.5 wrote -- undoing our own
;        past decision, while still leaving any default the person chose themselves alone.
; 0.1.7: an upgrade no longer demands a restart. The PIME text service DLLs were marked
;        ignoreversion, so every upgrade rewrote a byte-identical file that is mapped into every
;        running application; it could not be replaced, Inno scheduled it for the next boot, and
;        Setup asked the user to restart. Worse, a second install before that restart is refused
;        outright with "the installation of a previous program was not completed". Inno's normal
;        version check now skips those files when they already match.
; 0.1.6: installing no longer changes which input method you start in. Earlier versions forced the
;        default to Likhi, so every new window opened in Bangla and English needed a deliberate
;        switch in each application, which is the opposite of what a second keyboard should do.
;        Whatever the person has chosen in Settings is now left alone; Likhi is what you switch to
;        with Win+Space. Pass -MakeLikhiDefault to enable_keyboard.ps1 for the old behaviour.
; 0.1.5: the two reasons the keyboard never appeared on a colleague's machine.
;        (1) PIMETextService.dll enumerates the input methods to register from a path it builds
;            internally, %ProgramFiles(x86)%\PIME\python\input_methods, ignoring both its own
;            location and HKLM\SOFTWARE\PIME. Installing PIME under {app} therefore registered
;            nothing at all. Proven by registering the DLL from C:\Program Files\Likhi\pime, with
;            the registry key pointing there too, and watching it pick up a probe input method that
;            existed only under the hardcoded path. This worked on the developer's machine solely
;            because standalone PIME had been installed there years earlier.
;        (2) the profile check compared against a key name beginning '{{', because `{{` is the
;            escape for Inno constant expansion and does not apply inside a Pascal string literal.
;            The check could never pass, and it gated the per-user keyboard setup, so 0.1.2 reported
;            a failure that had not happened and then skipped the step that adds the keyboard.
; 0.1.3: keep the setup log. Inno writes it to %TEMP%, where Windows deletes it long before anyone
;        asks what went wrong, so every install now copies it to {app}\Setup.log, and a failed
;        install also drops it on the user's Desktop next to the diagnostics report.
; 0.1.2: register the text service from [Code] so failures are reported instead of swallowed by
;        regsvr32 /s, verify the language profile and the user's keyboard afterwards, and stop the
;        running engine with PowerShell rather than WMIC, which Windows 11 no longer ships.
; 0.1.1: the engine did not look for the shell's config.json in the installed layout, so a fresh
;        install never reported telemetry.
#define AppVersion "0.4.1"
#define AppPublisher "Khaled Bin Amir"
#define AppURL "https://github.com/KhaledBinAmir/likhi"
#define PimeSource "C:\Program Files (x86)\PIME"
; Where PIME must be installed, which is not negotiable: PIMETextService.dll builds this path
; internally and enumerates <PimeDir>\python\input_methods\*\ime.json there when regsvr32 calls
; DllRegisterServer. It ignores its own location and it ignores HKLM\SOFTWARE\PIME. Proven by
; registering the DLL from C:\Program Files\Likhi\pime with the registry key pointing at that same
; directory, and watching it register a probe input method that existed only under the path below.
#define PimeDir "{commonpf32}\PIME"

[Setup]
AppId={{7F2E5B91-4C3A-4D8E-9A61-2B7D5E8C4F30}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
DefaultDirName={commonpf}\Likhi
DefaultGroupName=Likhi
OutputDir=..\dist
OutputBaseFilename=LikhiSetup-{#AppVersion}
Compression=lzma2/max
SolidCompression=yes
; The text service DLL is loaded into every application, so this is a 64-bit install that also
; registers the 32-bit DLL for 32-bit applications.
ArchitecturesInstallIn64BitMode=x64compatible
ArchitecturesAllowed=x64compatible
PrivilegesRequired=admin
MinVersion=10.0.18362
UninstallDisplayName={#AppName} (Bangla phonetic keyboard)
WizardStyle=modern
LicenseFile=..\LICENSE
DisableProgramGroupPage=yes
; Our own processes are stopped in [Code]; the text service DLL is loaded inside every application
; that has had focus, and Restart Manager would offer to close the user's browser and editor to
; replace it. Scheduling a replacement at the next restart is far less disruptive, and the DLL is
; unchanged between Likhi releases anyway.
CloseApplications=no
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
; The engine: one native binary, the transliteration model, the lexicon tables and the Avro rules.
; It locates `models` relative to its own executable, so the two stay in the same directory.
Source: "..\dist\engine\*"; DestDir: "{app}\engine"; Flags: ignoreversion recursesubdirs createallsubdirs
; The text service. One DLL per architecture, because it is loaded into whichever process has
; keyboard focus and a 32-bit application cannot load the 64-bit one.
;
; No ignoreversion: these live inside every running application that has had focus. Forcing an
; overwrite locks the file, Inno schedules the replacement for the next boot, and Setup then asks
; for a restart on every upgrade. Inno's version check skips them when they already match.
; restartreplace remains as the fallback when a genuinely newer build cannot be written.
Source: "..\dist\shell\x64\LikhiTextService.dll"; DestDir: "{app}\shell\x64"; Flags: restartreplace uninsrestartdelete
Source: "..\dist\shell\x64\likhi.ico"; DestDir: "{app}\shell\x64"; Flags: ignoreversion
Source: "..\dist\shell\x86\LikhiTextService.dll"; DestDir: "{app}\shell\x86"; Flags: restartreplace uninsrestartdelete
Source: "..\dist\shell\x86\likhi.ico"; DestDir: "{app}\shell\x86"; Flags: ignoreversion
; Settings, with the pilot keys already stamped in by scripts/build_client.py. Read by both the
; engine and the text service; a per-user file in %LOCALAPPDATA%\Likhi overrides it key by key.
Source: "..\dist\likhi\config.json"; DestDir: "{app}"; Flags: ignoreversion
; Bangla faces for the candidate window, all under the SIL Open Font License. Installed properly so
; every application can use them, not only ours. fontisnttruetype is absent on purpose: these are
; TrueType, and Inno registers them and notifies running applications.
Source: "..\assets\fonts\NotoSansBengali.ttf"; DestDir: "{autofonts}"; FontInstall: "Noto Sans Bengali"; Flags: onlyifdoesntexist uninsneveruninstall
Source: "..\assets\fonts\AnekBangla.ttf"; DestDir: "{autofonts}"; FontInstall: "Anek Bangla"; Flags: onlyifdoesntexist uninsneveruninstall
Source: "..\assets\fonts\HindSiliguri-Regular.ttf"; DestDir: "{autofonts}"; FontInstall: "Hind Siliguri"; Flags: onlyifdoesntexist uninsneveruninstall
Source: "..\assets\fonts\TiroBangla-Regular.ttf"; DestDir: "{autofonts}"; FontInstall: "Tiro Bangla"; Flags: onlyifdoesntexist uninsneveruninstall
Source: "..\assets\fonts\*-OFL.txt"; DestDir: "{app}\fonts"; Flags: ignoreversion
; The window people open from the Start menu: status, how to switch, and the settings that are
; theirs to make. Needs no runtime of its own -- .NET Framework 4 is part of Windows.
; restartreplace as a safety net. This is the one file a person is likely to have open while
; upgrading, and a lock on it aborted the whole install and rolled it back. It is stopped by name in
; [Code] first; if something still holds it -- an antivirus scanning a file written seconds ago will
; -- the replacement is scheduled instead of the upgrade failing.
Source: "..\dist\Likhi.exe"; DestDir: "{app}"; Flags: ignoreversion restartreplace
; Per-user keyboard setup and documentation.
Source: "enable_keyboard.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "disable_keyboard.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "diagnose.ps1"; DestDir: "{app}"; Flags: ignoreversion
; Points Bangla at a chosen font across the whole machine, and puts it back. Run from the Likhi
; window, which raises the administrator prompt it needs.
Source: "system_font.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\THIRD-PARTY.md"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[InstallDelete]
; 0.1.4 and earlier put PIME under the application directory, where the text service could never
; find it. Remove that copy so an upgraded machine is not left with a second, useless PIME. A DLL
; that is still mapped into a running application cannot be deleted; Inno skips what it cannot
; remove and carries on, which is what we want -- deleting a mapped image is how you crash every
; program that has the keyboard loaded.
Type: filesandordirs; Name: "{app}\pime"
; 0.2.0 replaced PIME with our own text service. The files we put into the shared PIME directory go
; with it; the directory itself is left alone, because a machine may have installed PIME for its own
; reasons and Inno removes a directory only once it is empty.
Type: filesandordirs; Name: "{#PimeDir}\python\input_methods\likhi"

[Icons]
; First, and named just "Likhi", because that is what someone types into the Start menu when they
; want to know whether the thing they installed is working.
Name: "{group}\Likhi"; Filename: "{app}\Likhi.exe"
Name: "{group}\Likhi on GitHub"; Filename: "{#AppURL}"
; Any other user of this machine runs this once to get the keyboard and their own engine.
Name: "{group}\Set up the Likhi keyboard for this user"; Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\enable_keyboard.ps1"" -InstallDir ""{app}"""
Name: "{group}\Diagnose Likhi"; Filename: "powershell.exe"; Parameters: "-NoExit -NoProfile -ExecutionPolicy Bypass -File ""{app}\diagnose.ps1"""
Name: "{group}\Uninstall Likhi"; Filename: "{uninstallexe}"

[Registry]
; Autostart is per user and is written by enable_keyboard.ps1 under the account that will actually
; type, not here: an administrative install may run under a different account, and each user needs
; their own engine process so that personal learning data is never shared.
;
; LikhiLauncher is listed only so an upgrade from a PIME build removes it: there is no launcher any
; more, and a stale entry would start a program that is no longer installed at every sign-in.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "LikhiLauncher"; Flags: uninsdeletevalue deletevalue dontcreatekey
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "LikhiEngine"; Flags: uninsdeletevalue dontcreatekey

; Registration and keyboard setup happen in [Code] so their results can be checked and reported.
; Doing them here would hide a failure: regsvr32 /s is silent, and Windows drops an unregistered
; keyboard from the language list without an error, which is exactly how 0.1.1 failed quietly.

[UninstallRun]
; runasoriginaluser is a [Run]-only flag; the uninstaller already runs under the same user account,
; so HKCU (where the language list lives) is that user's hive.
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\disable_keyboard.ps1"""; Flags: waituntilterminated runhidden; RunOnceId: "RemoveKeyboard"
Filename: "{sys}\regsvr32.exe"; Parameters: "/u /s ""{app}\shell\x64\LikhiTextService.dll"""; Flags: waituntilterminated; RunOnceId: "UnregX64"
Filename: "{syswow64}\regsvr32.exe"; Parameters: "/u /s ""{app}\shell\x86\LikhiTextService.dll"""; Flags: waituntilterminated; RunOnceId: "UnregX86"

[Code]
const
  { Single braces on purpose. A doubled brace is the escape for Inno *constant expansion*, which
    applies to parameters in the file and run sections and to ExpandConstant -- never to a Pascal
    string literal, which is taken verbatim. Writing the doubled form here produced a lookup for a
    key name beginning with two braces, so the verification below could never pass, and 0.1.2 told
    every user that registration had failed when it had in fact succeeded.
    A section name in square brackets must not start a line in here either: Inno strips leading
    whitespace before deciding whether a line opens a new section, comment or not. }
  TipClsid = '{1D24C804-FAD0-4B32-AEDD-1317F4E6221E}';
  TipProfile = '{502AB3FE-5B7C-43E9-89D1-BE885846AE0D}';
  LangId = '0x00000845';
  { The PIME-based service that versions up to 0.1.12 installed. Kept here only so an upgrade can
    unregister it and take its entry out of the keyboard picker: leaving it would give people two
    Bangla keyboards, one of which no longer has any files behind it. }
  OldClsid = '{35F67E9D-A54D-4177-9697-8B0AB71A9E04}';
  OldProfile = '{9B4E7C21-3D5A-4F86-A2E1-6C0D8B7F5A13}';

function RunHidden(const Exe, Params: String; var Code: Integer): Boolean;
begin
  Result := Exec(Exe, Params, '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure StopOurProcesses();
var
  Code: Integer;
  Script: String;
begin
  { WMIC was removed from Windows 11, so an installer that relies on it silently fails to stop the
    running engine and then cannot overwrite its files. PowerShell is always present. Matching on
    the two install paths leaves the user's other Python processes alone; both are needed, because
    the engine runs from the application directory and PIME's Python backend runs from the PIME one.
    An Inno constant must never be written inside a comment like this: a Pascal comment ends at the
    first closing brace, so the constant's own brace would cut the comment short. }
  Script := '-NoProfile -ExecutionPolicy Bypass -Command "' +
            'Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -like ''' +
            ExpandConstant('{app}') + '\*'' -or $_.ExecutablePath -like ''' +
            ExpandConstant('{#PimeDir}') + '\*'' } | ForEach-Object { ' +
            'Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }"';
  RunHidden('powershell.exe', Script, Code);
  RunHidden(ExpandConstant('{sys}\taskkill.exe'), '/f /im PIMELauncher.exe', Code);
  { By name as well as by path. The Likhi window is the one file here a person is likely to have
    open while upgrading -- it is what they opened to find the download -- and the path match above
    depends on Win32_Process reporting ExecutablePath, which it does not always do. Without this an
    upgrade fails on "DeleteFile failed; code 5" and rolls itself back, which is exactly what it did
    on the development machine. }
  RunHidden(ExpandConstant('{sys}\taskkill.exe'), '/f /im Likhi.exe', Code);
  { The engine has been its own binary since 0.3. Stopped by name for the same reason as the window
    above: it holds its own executable and its mapped model files open, and an upgrade that cannot
    replace them rolls itself back. The Python engine is still stopped by path further up, because a
    machine upgrading from an older version has one running and it holds the port. }
  RunHidden(ExpandConstant('{sys}\taskkill.exe'), '/f /im likhi-server.exe', Code);
  Sleep(900);
end;

{ Remove the embedded Python engine an older version installed.

  Left alone it is a hundred megabytes of files nothing will ever run again, and worse, its
  likhi-server.cmd stays on the autostart path until the Likhi window happens to rewrite it -- so a
  machine could keep starting the old engine, which would hold port 47123 and the named pipe and
  quietly shadow the new one. }
procedure RemoveOldPythonRuntime();
begin
  if DirExists(ExpandConstant('{app}\runtime')) then
  begin
    Log('removing the Python runtime from an earlier version');
    DelTree(ExpandConstant('{app}\runtime'), True, True, True);
  end;
end;

{ Take the old PIME keyboard out of the picker before installing ours.
  Unregistered first, so the CLSID never points at files that are about to be deleted, and the
  profile key is removed by hand as well: regsvr32 /u cannot run once the DLL is gone, and an
  upgrade from a build whose files a previous uninstall already removed would otherwise leave a
  dead keyboard listed for ever. }
procedure RemoveOldTextService();
var
  Code: Integer;
begin
  if FileExists(ExpandConstant('{#PimeDir}\x64\PIMETextService.dll')) then
  begin
    RunHidden(ExpandConstant('{sys}\regsvr32.exe'),
              '/u /s "' + ExpandConstant('{#PimeDir}\x64\PIMETextService.dll') + '"', Code);
    RunHidden(ExpandConstant('{syswow64}\regsvr32.exe'),
              '/u /s "' + ExpandConstant('{#PimeDir}\x86\PIMETextService.dll') + '"', Code);
  end;
  RegDeleteKeyIncludingSubkeys(HKLM64, 'SOFTWARE\Microsoft\CTF\TIP\' + OldClsid);
  RegDeleteKeyIncludingSubkeys(HKLM32, 'SOFTWARE\Microsoft\CTF\TIP\' + OldClsid);
end;

function RegisterTextService(var Problem: String): Boolean;
var
  Code64, Code32: Integer;
  Desc: String;
begin
  RunHidden(ExpandConstant('{sys}\regsvr32.exe'),
            '/s "' + ExpandConstant('{app}\shell\x64\LikhiTextService.dll') + '"', Code64);
  RunHidden(ExpandConstant('{syswow64}\regsvr32.exe'),
            '/s "' + ExpandConstant('{app}\shell\x86\LikhiTextService.dll') + '"', Code32);
  if (Code64 <> 0) or (Code32 <> 0) then
  begin
    Problem := 'Registering the text service failed (64-bit code ' + IntToStr(Code64) +
               ', 32-bit code ' + IntToStr(Code32) + ').';
    Result := False;
    exit;
  end;

  { regsvr32 reports success whether or not it found an input method to register, so check that the
    profile is really there. Reading Description rather than testing for the key alone: an empty key
    left behind by an earlier attempt would satisfy RegKeyExists and tell us nothing. }
  if not RegQueryStringValue(HKLM64, 'SOFTWARE\Microsoft\CTF\TIP\' + TipClsid +
                             '\LanguageProfile\' + LangId + '\' + TipProfile, 'Description', Desc)
     or (Desc = '') then
  begin
    Problem := 'The text service registered but no Bangla keyboard profile was created.';
    Result := False;
    exit;
  end;
  Result := True;
end;

procedure SaveSetupLog(const AlsoToDesktop: Boolean);
var
  Src: String;
begin
  { Inno writes its log to %TEMP%, which Windows cleans out, so by the time a tester is asked what
    happened the single most informative file is already gone. Copy it somewhere that survives.
    The log is still open; Inno shares it for reading and only the last line or two can be missing. }
  Src := ExpandConstant('{log}');
  if Src = '' then
    exit;
  CopyFile(Src, ExpandConstant('{app}\Setup.log'), False);
  if AlsoToDesktop then
    CopyFile(Src, ExpandConstant('{userdesktop}\Likhi-setup-log.txt'), False);
end;

function KeyboardPresentForUser(): Boolean;
begin
  { Windows records a user's keyboards here. Checked directly rather than through PowerShell so the
    verification still works where Group Policy blocks scripts, which -ExecutionPolicy Bypass
    cannot override. }
  Result := RegKeyExists(HKCU, 'Control Panel\International\User Profile\bn-BD');
end;

function SetUpKeyboardForUser(): Boolean;
var
  Code: Integer;
begin
  ExecAsOriginalUser('powershell.exe',
    '-NoProfile -ExecutionPolicy Bypass -File "' + ExpandConstant('{app}\enable_keyboard.ps1') +
    '" -InstallDir "' + ExpandConstant('{app}') + '"',
    '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(500);
  Result := KeyboardPresentForUser();
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    StopOurProcesses();
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Problem: String;
  Code: Integer;
  Registered, HasKeyboard, Failed: Boolean;
begin
  { An upgrade must not leave the old engine holding its own files open, and must take the PIME
    keyboard out of the picker before its files are deleted underneath it. }
  if CurStep = ssInstall then
  begin
    StopOurProcesses();
    RemoveOldTextService();
    RemoveOldPythonRuntime();
    { Releases the CLSID from the 0.1.4-and-earlier location before that copy is deleted, so the
      registration never points at a path that no longer exists. Harmless on a first install. }
    if FileExists(ExpandConstant('{app}\pime\x64\PIMETextService.dll')) then
    begin
      RunHidden(ExpandConstant('{sys}\regsvr32.exe'),
                '/u /s "' + ExpandConstant('{app}\pime\x64\PIMETextService.dll') + '"', Code);
      RunHidden(ExpandConstant('{syswow64}\regsvr32.exe'),
                '/u /s "' + ExpandConstant('{app}\pime\x86\PIMETextService.dll') + '"', Code);
    end;
  end;

  if CurStep = ssPostInstall then
  begin
    Registered := RegisterTextService(Problem);
    { Attempted whether or not the check above passed. In 0.1.2 a faulty verification made this step
      conditional, so a false negative -- not a real failure -- was the reason the keyboard never
      reached anyone's language list. A verification exists to report, never to gate. }
    HasKeyboard := SetUpKeyboardForUser();
    Failed := (not HasKeyboard) or (not Registered);

    if not Registered then
      MsgBox('Likhi was copied to your computer, but the keyboard could not be registered.' + #13#10#13#10 +
             Problem + #13#10#13#10 +
             'Run "Diagnose Likhi" from the Start menu and send the report it saves on your ' +
             'Desktop, together with Likhi-setup-log.txt, to whoever gave you this installer.',
             mbError, MB_OK)
    else if not HasKeyboard then
      MsgBox('Likhi is installed and the keyboard is registered, but it could not be added to ' +
             'your language list automatically.' + #13#10#13#10 +
             'Open the Start menu and run "Set up the Likhi keyboard for this user", then press ' +
             'Win+Space.' + #13#10#13#10 +
             'If that does not work either, run "Diagnose Likhi" from the Start menu and send the ' +
             'report it saves on your Desktop, together with Likhi-setup-log.txt, to whoever gave ' +
             'you this installer.',
             mbInformation, MB_OK);

    { Written last so it captures the checks above, and put on the Desktop only when something went
      wrong: nobody wants a log file on their Desktop after an install that worked. }
    SaveSetupLog(Failed);
  end;
end;
