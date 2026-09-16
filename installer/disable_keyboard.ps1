<#
.SYNOPSIS
  Remove the Likhi keyboard from the current user's language list.

  Called by the uninstaller. Leaves Bengali itself in place if the user added other Bengali
  keyboards, and only removes the language when Likhi was its sole input method.
#>
$ErrorActionPreference = 'SilentlyContinue'

$tip = '0845:{35F67E9D-A54D-4177-9697-8B0AB71A9E04}{9B4E7C21-3D5A-4F86-A2E1-6C0D8B7F5A13}'

try {
    if ((Get-WinDefaultInputMethodOverride).InputMethodTip -eq $tip) {
        Set-WinDefaultInputMethodOverride   # back to the system default
    }
} catch {}

$list = Get-WinUserLanguageList
$bn = $list | Where-Object { $_.LanguageTag -eq 'bn-BD' }
if ($bn) {
    if ($bn.InputMethodTips -contains $tip) { [void]$bn.InputMethodTips.Remove($tip) }
    if ($bn.InputMethodTips.Count -eq 0 -and $list.Count -gt 1) {
        [void]$list.Remove($bn)
        Write-Host "removed Bengali (Bangladesh), it had no other keyboards"
    }
    Set-WinUserLanguageList $list -Force
    Write-Host "removed the Likhi keyboard"
}
