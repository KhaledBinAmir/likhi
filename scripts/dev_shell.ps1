<#
.SYNOPSIS
  Build the Rust text service, deploy it, register it, and restart the test application.

  The whole edit-test loop in one command:

    powershell -File scripts\dev_shell.ps1            # x64 only, fastest
    powershell -File scripts\dev_shell.ps1 -Both      # both architectures
    powershell -File scripts\dev_shell.ps1 -NoNotepad # skip relaunching Notepad

  A loaded DLL cannot be replaced and deleting one that is mapped crashes the process that mapped
  it, so each build goes to its own numbered folder under %LOCALAPPDATA%\Likhi\shell and the COM
  registration is repointed at it. Applications that are already running keep the DLL they have,
  which is why Notepad is closed and reopened here rather than left to be remembered.

  Registration needs an administrator, so this raises one UAC prompt per run.
#>
param(
    [switch]$Both,
    [switch]$NoNotepad,
    [switch]$Release
)

$ErrorActionPreference = 'Stop'
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

$repo = Split-Path -Parent $PSScriptRoot
$crate = Join-Path $repo 'shell'
$icon = Join-Path $repo 'windows\pime\likhi\icon.ico'
$profileDir = Join-Path $env:LOCALAPPDATA 'Likhi\shell'
$clsid = '{1D24C804-FAD0-4B32-AEDD-1317F4E6221E}'

$targets = @{ 'x64' = 'x86_64-pc-windows-msvc' }
if ($Both) { $targets['x86'] = 'i686-pc-windows-msvc' }
$profileName = if ($Release) { 'release' } else { 'debug' }

Push-Location $crate
try {
    foreach ($arch in $targets.Keys) {
        $args = @('build', '--target', $targets[$arch])
        if ($Release) { $args += '--release' }
        Write-Host "building $arch..."
        & cargo @args
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed for $arch" }
    }
} finally { Pop-Location }

# A new folder per run: the previous one may still be mapped into a running application.
$stamp = 'b{0}' -f (Get-Date -Format 'HHmmss')
$paths = @{}
foreach ($arch in $targets.Keys) {
    $dest = Join-Path $profileDir "$arch\$stamp"
    New-Item -ItemType Directory -Path $dest -Force | Out-Null
    Copy-Item (Join-Path $crate "target\$($targets[$arch])\$profileName\likhi_tsf.dll") (Join-Path $dest 'LikhiTextService.dll') -Force
    Copy-Item $icon (Join-Path $dest 'likhi.ico') -Force
    $paths[$arch] = Join-Path $dest 'LikhiTextService.dll'
    Write-Host ("deployed {0}: {1} KB" -f $arch, [math]::Round((Get-Item $paths[$arch]).Length / 1KB))
}

# Without this a dev build is invisible to every Store application, and the symptom does not look
# like a permissions problem at all: the keyboard simply cannot be selected in Telegram or WhatsApp,
# and Windows drops the window back to ENG. An AppContainer gets no access to %LOCALAPPDATA% by
# default, so it cannot read the DLL, so TSF cannot load the text service. The installer's own
# directory under Program Files already carries these two ACEs, which is why an installed build
# works and a dev build silently does not -- an entire evening was spent chasing that difference.
#
# Both package SIDs, matching the engine pipe: AC covers an ordinary Store application, and
# S-1-15-2-2 (ALL RESTRICTED APPLICATION PACKAGES) covers a Less Privileged AppContainer, which
# carries no AC SID at all.
foreach ($arch in $targets.Keys) {
    $dir = Split-Path -Parent $paths[$arch]
    foreach ($sid in @('*S-1-15-2-1', '*S-1-15-2-2')) {
        & icacls $dir /grant "${sid}:(OI)(CI)(RX)" /T /Q | Out-Null
    }
}
# The whole chain has to be traversable, not just the leaf: no access on a parent means no access
# to anything under it, however the leaf is ACLed.
foreach ($sid in @('*S-1-15-2-1', '*S-1-15-2-2')) {
    & icacls $profileDir /grant "${sid}:(RX)" /Q | Out-Null
    & icacls (Split-Path -Parent $profileDir) /grant "${sid}:(RX)" /Q | Out-Null
}
Write-Host "granted app-package read on the deploy folders"

if (-not $NoNotepad) {
    Get-Process Notepad -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Host "closing Notepad pid=$($_.Id)"
        $_.CloseMainWindow() | Out-Null
    }
    Start-Sleep -Milliseconds 700
    Get-Process Notepad -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

$commands = @()
$commands += "Start-Process regsvr32.exe -ArgumentList '/s','`"$($paths['x64'])`"' -Wait"
if ($paths.ContainsKey('x86')) {
    $commands += "Start-Process '$env:SystemRoot\SysWOW64\regsvr32.exe' -ArgumentList '/s','`"$($paths['x86'])`"' -Wait"
}
$p = Start-Process powershell -ArgumentList '-NoProfile', '-Command', ($commands -join '; ') -Verb RunAs -Wait -PassThru
if ($p.ExitCode -ne 0) { throw "registration failed with exit code $($p.ExitCode)" }

$registered = (Get-ItemProperty "HKLM:\SOFTWARE\Classes\CLSID\$clsid\InprocServer32" -ErrorAction SilentlyContinue).'(default)'
Write-Host "registered: $registered"
$prof = "HKLM:\SOFTWARE\Microsoft\CTF\TIP\$clsid\LanguageProfile\0x00000845\{502AB3FE-5B7C-43E9-89D1-BE885846AE0D}"
Write-Host "picker name: $((Get-ItemProperty $prof -ErrorAction SilentlyContinue).Description)"

# The pipe first, because that is what the shell now tries first and the only transport a Store
# application can use; the socket is checked too, since it is still the fallback.
$session = (Get-Process -Id $PID).SessionId
try {
    $p = New-Object IO.Pipes.NamedPipeClientStream('.', "likhi-engine-s$session", [IO.Pipes.PipeDirection]::InOut)
    $p.Connect(500)
    $w = New-Object IO.StreamWriter($p); $r = New-Object IO.StreamReader($p)
    $w.WriteLine('{"op":"ping"}'); $w.Flush()
    Write-Host "engine (pipe): $($r.ReadLine())"; $p.Close()
} catch { Write-Host "engine NOT responding on \\.\pipe\likhi-engine-s$session" }

try {
    $c = New-Object Net.Sockets.TcpClient('127.0.0.1', 47123)
    $s = $c.GetStream(); $w = New-Object IO.StreamWriter($s); $r = New-Object IO.StreamReader($s)
    $w.WriteLine('{"op":"ping"}'); $w.Flush()
    Write-Host "engine (socket): $($r.ReadLine())"; $c.Close()
} catch { Write-Host "engine NOT responding on 127.0.0.1:47123" }

if (-not $NoNotepad) {
    Start-Process notepad.exe
    Write-Host "Notepad reopened on the new build. Win+Space, then type."
}
