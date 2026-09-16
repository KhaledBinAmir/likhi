; Likhi installer. Build with:
;   python scripts/build_runtime.py
;   python scripts/build_client.py
;   "%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe" installer\likhi.iss
;
; Produces dist\LikhiSetup-<version>.exe: one per-machine installer that needs administrator
; rights once and leaves the user with a working Bangla keyboard and nothing to configure.

#define AppName "Likhi"
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
#define AppVersion "0.1.9"
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
; The engine: embedded Python, NumPy, the Likhi package, the model and the lexicon.
Source: "..\dist\runtime\*"; DestDir: "{app}\runtime"; Flags: ignoreversion recursesubdirs createallsubdirs
; PIME: the Text Services Framework host. Unmodified, LGPL-2.1, see THIRD-PARTY.md.
;
; Installed to {#PimeDir} rather than under {app}, because the DLL only ever looks there -- see the
; note beside the PimeDir definition. Until 0.1.4 this went to {app}\pime, regsvr32 found no input
; methods to register, no language profile was written, and the keyboard never appeared for anyone
; who had not previously installed PIME by hand.
;
; Inno removes only the files it installed and only removes a directory once it is empty, so sharing
; this directory with an existing standalone PIME installation is safe in both directions.
Source: "{#PimeSource}\PIMELauncher.exe"; DestDir: "{#PimeDir}"; Flags: ignoreversion restartreplace
Source: "{#PimeSource}\backends.json"; DestDir: "{#PimeDir}"; Flags: ignoreversion
Source: "{#PimeSource}\version.txt"; DestDir: "{#PimeDir}"; Flags: ignoreversion
; No ignoreversion here, unlike everything else we ship. These DLLs live inside every running
; application that has had keyboard focus. Forcing an overwrite means the file is locked, Inno
; schedules the replacement for the next boot, and Setup then tells the user to restart -- on every
; single upgrade, to install a byte-identical file. Inno's normal version check skips them when they
; already match, so an upgrade touches nothing that is in use and needs no restart, while a genuinely
; newer PIME would still be installed. restartreplace remains as the fallback for the first install
; on a machine that already had PIME running.
Source: "{#PimeSource}\x64\*"; DestDir: "{#PimeDir}\x64"; Flags: recursesubdirs restartreplace uninsrestartdelete
Source: "{#PimeSource}\x86\*"; DestDir: "{#PimeDir}\x86"; Flags: recursesubdirs restartreplace uninsrestartdelete
; PIME's Python backend host, without the input methods we do not ship.
Source: "{#PimeSource}\python\*"; DestDir: "{#PimeDir}\python"; Flags: ignoreversion recursesubdirs createallsubdirs; Excludes: "input_methods\*,__pycache__"
Source: "{#PimeSource}\python\input_methods\*.py"; DestDir: "{#PimeDir}\python\input_methods"; Flags: ignoreversion skipifsourcedoesntexist
; Our text service, with the pilot keys already stamped in by scripts/build_client.py.
Source: "..\dist\likhi\*"; DestDir: "{#PimeDir}\python\input_methods\likhi"; Flags: ignoreversion
; Per-user keyboard setup and documentation.
Source: "enable_keyboard.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "disable_keyboard.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "diagnose.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\THIRD-PARTY.md"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[InstallDelete]
; 0.1.4 and earlier put PIME under the application directory, where the text service could never
; find it. Remove that copy so an upgraded machine is not left with a second, useless PIME. A DLL
; that is still mapped into a running application cannot be deleted; Inno skips what it cannot
; remove and carries on, which is what we want -- deleting a mapped image is how you crash every
; program that has the keyboard loaded.
Type: filesandordirs; Name: "{app}\pime"

[Icons]
Name: "{group}\Likhi on GitHub"; Filename: "{#AppURL}"
; Any other user of this machine runs this once to get the keyboard and their own engine.
Name: "{group}\Set up the Likhi keyboard for this user"; Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\enable_keyboard.ps1"" -InstallDir ""{app}"" -PimeDir ""{#PimeDir}"""
Name: "{group}\Diagnose Likhi"; Filename: "powershell.exe"; Parameters: "-NoExit -NoProfile -ExecutionPolicy Bypass -File ""{app}\diagnose.ps1"""
Name: "{group}\Uninstall Likhi"; Filename: "{uninstallexe}"

[Registry]
; Autostart is per user and is written by enable_keyboard.ps1 under the account that will actually
; type, not here: an administrative install may run under a different account, and each user needs
; their own engine process so that personal learning data is never shared.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "LikhiLauncher"; Flags: uninsdeletevalue dontcreatekey
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "LikhiEngine"; Flags: uninsdeletevalue dontcreatekey

; Registration and keyboard setup happen in [Code] so their results can be checked and reported.
; Doing them here would hide a failure: regsvr32 /s is silent, and Windows drops an unregistered
; keyboard from the language list without an error, which is exactly how 0.1.1 failed quietly.

[UninstallRun]
; HKLM\SOFTWARE\PIME is deliberately left behind. It is PIME's own key, not ours; on a machine that
; also has standalone PIME installed, deleting it would break that installation, and a value left
; pointing at a removed directory is the milder of the two failures.
;
; runasoriginaluser is a [Run]-only flag; the uninstaller already runs under the same user account,
; so HKCU (where the language list lives) is that user's hive.
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\disable_keyboard.ps1"""; Flags: waituntilterminated runhidden; RunOnceId: "RemoveKeyboard"
Filename: "{sys}\taskkill.exe"; Parameters: "/f /im PIMELauncher.exe"; Flags: waituntilterminated runhidden; RunOnceId: "StopLauncher"
Filename: "{sys}\regsvr32.exe"; Parameters: "/u /s ""{#PimeDir}\x64\PIMETextService.dll"""; Flags: waituntilterminated; RunOnceId: "UnregX64"
Filename: "{syswow64}\regsvr32.exe"; Parameters: "/u /s ""{#PimeDir}\x86\PIMETextService.dll"""; Flags: waituntilterminated; RunOnceId: "UnregX86"

[Code]
const
  { Single braces on purpose. A doubled brace is the escape for Inno *constant expansion*, which
    applies to parameters in the file and run sections and to ExpandConstant -- never to a Pascal
    string literal, which is taken verbatim. Writing the doubled form here produced a lookup for a
    key name beginning with two braces, so the verification below could never pass, and 0.1.2 told
    every user that registration had failed when it had in fact succeeded.
    A section name in square brackets must not start a line in here either: Inno strips leading
    whitespace before deciding whether a line opens a new section, comment or not. }
  TipClsid = '{35F67E9D-A54D-4177-9697-8B0AB71A9E04}';
  TipProfile = '{9B4E7C21-3D5A-4F86-A2E1-6C0D8B7F5A13}';
  LangId = '0x00000845';

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
  Sleep(700);
end;

function RegisterTextService(var Problem: String): Boolean;
var
  Code64, Code32: Integer;
  Desc: String;
begin
  { PIMELauncher reads this to find its own files. The DLL does not -- it uses a path it builds
    internally -- so writing this does not help registration, but leaving it wrong would break the
    launcher on a machine that once had PIME somewhere else. }
  RegWriteStringValue(HKLM, 'SOFTWARE\PIME', '', ExpandConstant('{#PimeDir}'));

  RunHidden(ExpandConstant('{sys}\regsvr32.exe'),
            '/s "' + ExpandConstant('{#PimeDir}\x64\PIMETextService.dll') + '"', Code64);
  RunHidden(ExpandConstant('{syswow64}\regsvr32.exe'),
            '/s "' + ExpandConstant('{#PimeDir}\x86\PIMETextService.dll') + '"', Code32);
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
    '" -InstallDir "' + ExpandConstant('{app}') +
    '" -PimeDir "' + ExpandConstant('{#PimeDir}') + '"',
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
  { An upgrade must not leave the old engine holding its own files open. }
  if CurStep = ssInstall then
  begin
    StopOurProcesses();
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
