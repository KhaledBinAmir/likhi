<#
.SYNOPSIS
  Remove the Likhi keyboard from the current user's language list.

  Called by the uninstaller. Leaves Bengali itself in place if the user added other Bengali
  keyboards, and only removes the language when Likhi was its sole input method.
#>
$ErrorActionPreference = 'SilentlyContinue'

# Both services: the current one, and the PIME-based one shipped up to 0.1.12, so uninstalling
# after an upgrade cannot leave a dead keyboard listed.
$tips = @(
    '0845:{1D24C804-FAD0-4B32-AEDD-1317F4E6221E}{502AB3FE-5B7C-43E9-89D1-BE885846AE0D}',
    '0845:{35F67E9D-A54D-4177-9697-8B0AB71A9E04}{9B4E7C21-3D5A-4F86-A2E1-6C0D8B7F5A13}'
)

try {
    $current = (Get-WinDefaultInputMethodOverride).InputMethodTip
    if ($tips -contains $current) {
        Set-WinDefaultInputMethodOverride   # back to the system default
    }
} catch {}

$list = Get-WinUserLanguageList
$bn = $list | Where-Object { $_.LanguageTag -eq 'bn-BD' }
if ($bn) {
    foreach ($tip in $tips) {
        if ($bn.InputMethodTips -contains $tip) { [void]$bn.InputMethodTips.Remove($tip) }
    }
    if ($bn.InputMethodTips.Count -eq 0 -and $list.Count -gt 1) {
        [void]$list.Remove($bn)
        Write-Host "removed Bengali (Bangladesh), it had no other keyboards"
    }
    Set-WinUserLanguageList $list -Force
    Write-Host "removed the Likhi keyboard"
}
