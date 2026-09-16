; Likhi installer. Build with:
;   python scripts/build_runtime.py
;   python scripts/build_client.py
;   "%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe" installer\likhi.iss
;
; Produces dist\LikhiSetup-<version>.exe: one per-machine installer that needs administrator
; rights once and leaves the user with a working Bangla keyboard and nothing to configure.

#define AppName "Likhi"
; 0.1.2: register the text service from [Code] so failures are reported instead of swallowed by
;        regsvr32 /s, verify the language profile and the user's keyboard afterwards, and stop the
;        running engine with PowerShell rather than WMIC, which Windows 11 no longer ships.
; 0.1.1: the engine did not look for the shell's config.json in the installed layout, so a fresh
;        install never reported telemetry.
#define AppVersion "0.1.2"
#define AppPublisher "Khaled Bin Amir"
#define AppURL "https://github.com/KhaledBinAmir/likhi"
#define PimeSource "C:\Program Files (x86)\PIME"

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
Source: "{#PimeSource}\PIMELauncher.exe"; DestDir: "{app}\pime"; Flags: ignoreversion restartreplace
Source: "{#PimeSource}\backends.json"; DestDir: "{app}\pime"; Flags: ignoreversion
Source: "{#PimeSource}\version.txt"; DestDir: "{app}\pime"; Flags: ignoreversion
; restartreplace: this DLL lives inside every running application that has had focus, so an upgrade
; must not fail when it cannot be overwritten. It is identical between Likhi releases.
Source: "{#PimeSource}\x64\*"; DestDir: "{app}\pime\x64"; Flags: ignoreversion recursesubdirs restartreplace uninsrestartdelete
Source: "{#PimeSource}\x86\*"; DestDir: "{app}\pime\x86"; Flags: ignoreversion recursesubdirs restartreplace uninsrestartdelete
; PIME's Python backend host, without the input methods we do not ship.
Source: "{#PimeSource}\python\*"; DestDir: "{app}\pime\python"; Flags: ignoreversion recursesubdirs createallsubdirs; Excludes: "input_methods\*,__pycache__"
Source: "{#PimeSource}\python\input_methods\*.py"; DestDir: "{app}\pime\python\input_methods"; Flags: ignoreversion skipifsourcedoesntexist
; Our text service, with the pilot keys already stamped in by scripts/build_client.py.
Source: "..\dist\likhi\*"; DestDir: "{app}\pime\python\input_methods\likhi"; Flags: ignoreversion
; Per-user keyboard setup and documentation.
Source: "enable_keyboard.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "disable_keyboard.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "diagnose.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\THIRD-PARTY.md"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Likhi on GitHub"; Filename: "{#AppURL}"
; Any other user of this machine runs this once to get the keyboard and their own engine.
Name: "{group}\Set up the Likhi keyboard for this user"; Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\enable_keyboard.ps1"" -InstallDir ""{app}"""
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
; runasoriginaluser is a [Run]-only flag; the uninstaller already runs under the same user account,
; so HKCU (where the language list lives) is that user's hive.
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\disable_keyboard.ps1"""; Flags: waituntilterminated runhidden; RunOnceId: "RemoveKeyboard"
Filename: "{sys}\taskkill.exe"; Parameters: "/f /im PIMELauncher.exe"; Flags: waituntilterminated runhidden; RunOnceId: "StopLauncher"
Filename: "{sys}\regsvr32.exe"; Parameters: "/u /s ""{app}\pime\x64\PIMETextService.dll"""; Flags: waituntilterminated; RunOnceId: "UnregX64"
Filename: "{syswow64}\regsvr32.exe"; Parameters: "/u /s ""{app}\pime\x86\PIMETextService.dll"""; Flags: waituntilterminated; RunOnceId: "UnregX86"

[Code]
const
  TipClsid = '{{35F67E9D-A54D-4177-9697-8B0AB71A9E04}';
  TipProfile = '{{9B4E7C21-3D5A-4F86-A2E1-6C0D8B7F5A13}';

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
    the install path leaves the user's other Python processes alone. }
  Script := '-NoProfile -ExecutionPolicy Bypass -Command "' +
            'Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -like ''' +
            ExpandConstant('{app}') + '\*'' } | ForEach-Object { ' +
            'Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }"';
  RunHidden('powershell.exe', Script, Code);
  RunHidden(ExpandConstant('{sys}\taskkill.exe'), '/f /im PIMELauncher.exe', Code);
  Sleep(700);
end;

function RegisterTextService(var Problem: String): Boolean;
var
  Code64, Code32: Integer;
begin
  RunHidden(ExpandConstant('{sys}\regsvr32.exe'),
            '/s "' + ExpandConstant('{app}\pime\x64\PIMETextService.dll') + '"', Code64);
  RunHidden(ExpandConstant('{syswow64}\regsvr32.exe'),
            '/s "' + ExpandConstant('{app}\pime\x86\PIMETextService.dll') + '"', Code32);
  if (Code64 <> 0) or (Code32 <> 0) then
  begin
    Problem := 'Registering the text service failed (64-bit code ' + IntToStr(Code64) +
               ', 32-bit code ' + IntToStr(Code32) + ').';
    Result := False;
    exit;
  end;
  { regsvr32 can report success while writing no language profile, for example when it cannot read
    ime.json. Verify what matters rather than trusting the exit code. }
  if not RegKeyExists(HKLM64, 'SOFTWARE\Microsoft\CTF\TIP\' + TipClsid +
                              '\LanguageProfile\0x00000845\' + TipProfile) then
  begin
    Problem := 'The text service registered but no Bangla keyboard profile was created.';
    Result := False;
    exit;
  end;
  Result := True;
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
begin
  { An upgrade must not leave the old engine holding its own files open. }
  if CurStep = ssInstall then
    StopOurProcesses();

  if CurStep = ssPostInstall then
  begin
    if not RegisterTextService(Problem) then
      MsgBox('Likhi was copied to your computer, but the keyboard could not be registered.' + #13#10#13#10 +
             Problem + #13#10#13#10 +
             'The keyboard will not appear until this is fixed. Run "Diagnose Likhi" from the ' +
             'Start menu and send the output to whoever gave you this installer.',
             mbError, MB_OK)
    else if not SetUpKeyboardForUser() then
      MsgBox('Likhi is installed and the keyboard is registered, but it could not be added to ' +
             'your language list automatically.' + #13#10#13#10 +
             'Open the Start menu and run "Set up the Likhi keyboard for this user", then press ' +
             'Win+Space.' + #13#10#13#10 +
             'If that does not work either, run "Diagnose Likhi" from the Start menu and send the ' +
             'output to whoever gave you this installer.',
             mbInformation, MB_OK);
  end;
end;
