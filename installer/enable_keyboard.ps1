<#
.SYNOPSIS
  Add the Likhi keyboard to the current user's language list, changing nothing else.

  Run per user, not per machine: Windows stores input methods in the user profile, so a
  machine-wide install still needs this once for each person who will type Bangla.

  Whichever input method the person starts in is left exactly as they have it in Settings.
  Pass -MakeLikhiDefault to start every window in Bangla instead.
#>
param(
    # Off by default, and deliberately so. Windows starts every new window in the default input
    # method, so making Likhi the default means English needs a deliberate switch in every
    # application -- the opposite of what anyone expects from a second keyboard. A Bangla keyboard
    # should be there when you reach for it, not in the way when you do not.
    [switch]$MakeLikhiDefault,
    [string]$InstallDir = (Split-Path -Parent $PSCommandPath)
)

$ErrorActionPreference = 'Stop'

# Our text service CLSID and language profile, from shell/src/guids.rs.
$tip = '0845:{1D24C804-FAD0-4B32-AEDD-1317F4E6221E}{502AB3FE-5B7C-43E9-89D1-BE885846AE0D}'
# The PIME-based service shipped up to 0.1.12. Removed from the language list on upgrade so nobody
# is left with two Bangla keyboards, one of which has no files behind it any more.
$oldTip = '0845:{35F67E9D-A54D-4177-9697-8B0AB71A9E04}{9B4E7C21-3D5A-4F86-A2E1-6C0D8B7F5A13}'

$list = Get-WinUserLanguageList
$bn = $list | Where-Object { $_.LanguageTag -eq 'bn-BD' }
if (-not $bn) {
    $list.Add('bn-BD')
    $bn = $list | Where-Object { $_.LanguageTag -eq 'bn-BD' }
    Write-Host "added Bengali (Bangladesh) to the language list"
}
if ($bn.InputMethodTips -notcontains $tip) {
    $bn.InputMethodTips.Add($tip)
    Write-Host "added the Likhi keyboard"
} else {
    Write-Host "the Likhi keyboard was already present"
}

# Everything else goes: the previous PIME-based keyboard, and Bengali INSCRIPT.
#
# Adding bn-BD makes Windows attach that language's default physical layout as well, Bengali
# INSCRIPT (0845:00000445). It then sits next to Likhi in Win+Space, and INSCRIPT maps QWERTY keys
# straight onto Bangla letters, so anyone who lands on it types what looks like gibberish and
# concludes the keyboard is broken. Two pilot machines hit exactly this. Bangla here means Likhi;
# a person who genuinely wants INSCRIPT can add it in Settings.
for ($i = $bn.InputMethodTips.Count - 1; $i -ge 0; $i--) {
    if ($bn.InputMethodTips[$i] -ne $tip) {
        Write-Host "removing $($bn.InputMethodTips[$i]) from Bengali (Bangladesh)"
        $bn.InputMethodTips.RemoveAt($i)
    }
}

Set-WinUserLanguageList $list -Force

if ($MakeLikhiDefault) {
    Set-WinDefaultInputMethodOverride -InputTip $tip
    Write-Host "Likhi is now the default input method"
} else {
    # Installing a keyboard is not a reason to change which one someone starts in, and whatever they
    # chose in Settings is not ours to overwrite. The one exception is an override we set ourselves:
    # versions up to 0.1.5 forced the default to Likhi, so every new window opened in Bangla. Undoing
    # our own past decision is fair; touching anything else is not.
    $current = $null
    try { $current = (Get-WinDefaultInputMethodOverride).InputMethodTip } catch { }
    if ($current -eq $tip) {
        Set-WinDefaultInputMethodOverride
        Write-Host "cleared the Likhi default that an earlier version set; new windows use your own default again"
    } else {
        Write-Host "default input method left as you have it; press Win+Space for Likhi"
    }
}

# Start at sign-in, per user. Each person gets their own engine process and therefore their own
# learning data; a machine-wide entry would share one engine, and one person's personal
# dictionary, between everyone signed in.
#
# One entry now, not two: the text service is a DLL that Windows loads into each application by
# itself, so there is no launcher process to start any more. Any LikhiLauncher entry left by an
# earlier version is removed, or it would try to run a program that is no longer installed.
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
Remove-ItemProperty -Path $run -Name 'LikhiLauncher' -ErrorAction SilentlyContinue
$engine = Join-Path $InstallDir 'engine\likhi-server.exe'
if (Test-Path $engine) {
    Set-ItemProperty -Path $run -Name 'LikhiEngine' -Value """$engine"""
    Write-Host "set Likhi to start when you sign in"
    Start-Process -FilePath $engine -WorkingDirectory (Split-Path $engine) -WindowStyle Hidden
    Write-Host "started the suggestion engine"
} else {
    Write-Host "WARNING: engine not found at $engine"
}

Write-Host "DONE. Press Win+Space to switch keyboards."
