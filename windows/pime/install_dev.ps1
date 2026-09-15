<#
.SYNOPSIS
  Developer install of Likhi on top of PIME (elevated part).

  1. Installs PIME silently from the given installer (if not already installed).
  2. Copies the Likhi text-service folder into PIME's python input_methods.
  3. Re-registers PIMETextService.dll (x64 + x86) so the new ime.json profile is registered.
  4. Restarts PIMELauncher so the Python backend picks up the new module.

  Run elevated. The non-elevated steps (adding the keyboard to the user's language list and
  starting likhi-server) are done by the caller; see README.md.
#>
param(
    [string]$Installer = "",
    [string]$Source = (Join-Path $PSScriptRoot "likhi")
)

$ErrorActionPreference = "Stop"
$pime = "C:\Program Files (x86)\PIME"

if (-not (Test-Path "$pime\x64\PIMETextService.dll")) {
    if (-not $Installer -or -not (Test-Path $Installer)) { throw "PIME is not installed and no installer was given: $Installer" }
    Write-Host "Installing PIME silently from $Installer ..."
    $p = Start-Process -FilePath $Installer -ArgumentList "/S" -Wait -PassThru
    Write-Host "installer exit code: $($p.ExitCode)"
    if (-not (Test-Path "$pime\x64\PIMETextService.dll")) { throw "PIME install did not produce $pime\x64\PIMETextService.dll" }
} else {
    Write-Host "PIME already installed at $pime"
}

$dest = "$pime\python\input_methods\likhi"
Write-Host "Copying $Source -> $dest"
New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item -Path (Join-Path $Source "*") -Destination $dest -Recurse -Force

Write-Host "Stopping PIMELauncher ..."
Get-Process PIMELauncher -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500

foreach ($arch in @("x64", "x86")) {
    $dll = "$pime\$arch\PIMETextService.dll"
    if (Test-Path $dll) {
        Write-Host "regsvr32 $dll"
        $r = Start-Process -FilePath "$env:WINDIR\System32\regsvr32.exe" -ArgumentList "/s", "`"$dll`"" -Wait -PassThru
        Write-Host "  exit code: $($r.ExitCode)"
    }
}

Write-Host "Starting PIMELauncher ..."
Start-Process -FilePath "$pime\PIMELauncher.exe" -WorkingDirectory $pime

$profileGuid = "{9B4E7C21-3D5A-4F86-A2E1-6C0D8B7F5A13}"
$found = Get-ChildItem "HKLM:\SOFTWARE\Microsoft\CTF\TIP" | ForEach-Object {
    $lp = Join-Path $_.PSPath "LanguageProfile"
    if (Test-Path $lp) {
        Get-ChildItem $lp | ForEach-Object {
            if (Test-Path (Join-Path $_.PSPath $profileGuid)) { "$($_.PSChildName) $($_.PSParentPath.Split('\')[-2])" }
        }
    }
}
Write-Host "Likhi profile registered under: $found"
Write-Host "DONE"
