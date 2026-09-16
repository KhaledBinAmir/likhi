; Likhi installer. Build with:
;   python scripts/build_runtime.py
;   python scripts/build_client.py
;   "%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe" installer\likhi.iss
;
; Produces dist\LikhiSetup-<version>.exe: one per-machine installer that needs administrator
; rights once and leaves the user with a working Bangla keyboard and nothing to configure.

#define AppName "Likhi"
; 0.1.1 fixes a silent failure in 0.1.0: the engine did not look for the shell's config.json in the
; installed layout, so a fresh install never reported telemetry.
#define AppVersion "0.1.1"
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

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
; The engine: embedded Python, NumPy, the Likhi package, the model and the lexicon.
Source: "..\dist\runtime\*"; DestDir: "{app}\runtime"; Flags: ignoreversion recursesubdirs createallsubdirs
; PIME: the Text Services Framework host. Unmodified, LGPL-2.1, see THIRD-PARTY.md.
Source: "{#PimeSource}\PIMELauncher.exe"; DestDir: "{app}\pime"; Flags: ignoreversion
Source: "{#PimeSource}\backends.json"; DestDir: "{app}\pime"; Flags: ignoreversion
Source: "{#PimeSource}\version.txt"; DestDir: "{app}\pime"; Flags: ignoreversion
Source: "{#PimeSource}\x64\*"; DestDir: "{app}\pime\x64"; Flags: ignoreversion recursesubdirs
Source: "{#PimeSource}\x86\*"; DestDir: "{app}\pime\x86"; Flags: ignoreversion recursesubdirs
; PIME's Python backend host, without the input methods we do not ship.
Source: "{#PimeSource}\python\*"; DestDir: "{app}\pime\python"; Flags: ignoreversion recursesubdirs createallsubdirs; Excludes: "input_methods\*,__pycache__"
Source: "{#PimeSource}\python\input_methods\*.py"; DestDir: "{app}\pime\python\input_methods"; Flags: ignoreversion skipifsourcedoesntexist
; Our text service, with the pilot keys already stamped in by scripts/build_client.py.
Source: "..\dist\likhi\*"; DestDir: "{app}\pime\python\input_methods\likhi"; Flags: ignoreversion
; Per-user keyboard setup and documentation.
Source: "enable_keyboard.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "disable_keyboard.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\THIRD-PARTY.md"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Likhi on GitHub"; Filename: "{#AppURL}"
; Any other user of this machine runs this once to get the keyboard and their own engine.
Name: "{group}\Set up the Likhi keyboard for this user"; Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\enable_keyboard.ps1"" -InstallDir ""{app}"""
Name: "{group}\Uninstall Likhi"; Filename: "{uninstallexe}"

[Registry]
; Autostart is per user and is written by enable_keyboard.ps1 under the account that will actually
; type, not here: an administrative install may run under a different account, and each user needs
; their own engine process so that personal learning data is never shared.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "LikhiLauncher"; Flags: uninsdeletevalue dontcreatekey
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "LikhiEngine"; Flags: uninsdeletevalue dontcreatekey

[Run]
; Register the text service for 64-bit and 32-bit applications.
Filename: "{sys}\regsvr32.exe"; Parameters: "/s ""{app}\pime\x64\PIMETextService.dll"""; StatusMsg: "Registering the text service (64-bit)..."; Flags: waituntilterminated
Filename: "{syswow64}\regsvr32.exe"; Parameters: "/s ""{app}\pime\x86\PIMETextService.dll"""; StatusMsg: "Registering the text service (32-bit)..."; Flags: waituntilterminated
; Add the keyboard, set autostart and start everything for the person installing, as that person
; rather than as the elevated account. Other users run the Start menu shortcut once.
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\enable_keyboard.ps1"" -InstallDir ""{app}"""; StatusMsg: "Adding the Bangla keyboard..."; Flags: waituntilterminated runasoriginaluser runhidden

[UninstallRun]
; runasoriginaluser is a [Run]-only flag; the uninstaller already runs under the same user account,
; so HKCU (where the language list lives) is that user's hive.
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\disable_keyboard.ps1"""; Flags: waituntilterminated runhidden; RunOnceId: "RemoveKeyboard"
Filename: "{sys}\taskkill.exe"; Parameters: "/f /im PIMELauncher.exe"; Flags: waituntilterminated runhidden; RunOnceId: "StopLauncher"
Filename: "{sys}\regsvr32.exe"; Parameters: "/u /s ""{app}\pime\x64\PIMETextService.dll"""; Flags: waituntilterminated; RunOnceId: "UnregX64"
Filename: "{syswow64}\regsvr32.exe"; Parameters: "/u /s ""{app}\pime\x86\PIMETextService.dll"""; Flags: waituntilterminated; RunOnceId: "UnregX86"

[Code]
function StopEngine(): Boolean;
var
  ResultCode: Integer;
begin
  { The engine runs as pythonw.exe from our own runtime folder; match on that path so a user's
    other Python processes are untouched. }
  Exec(ExpandConstant('{sys}\wbem\WMIC.exe'),
       'process where "ExecutablePath like ''' + ExpandConstant('{app}') + '%pythonw.exe''" delete',
       '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    StopEngine();
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  { An upgrade must not leave the old engine holding the model files open. }
  if CurStep = ssInstall then
    StopEngine();
end;
