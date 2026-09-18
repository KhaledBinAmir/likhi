<#
.SYNOPSIS
  Report why the Likhi keyboard is or is not working on this machine.

  Run on any machine, no admin needed, and send back the file it writes. It reads state only and
  changes nothing.

    powershell -NoProfile -ExecutionPolicy Bypass -File diagnose.ps1

  Everything printed is also saved to a text file on the Desktop, because asking a tester to
  copy-paste a console window loses half the output and all of the formatting.
#>

$clsid = '{1D24C804-FAD0-4B32-AEDD-1317F4E6221E}'
$profileGuid = '{502AB3FE-5B7C-43E9-89D1-BE885846AE0D}'
$tip = "0845:$clsid$profileGuid"
# The PIME-based service shipped up to 0.1.12. Reported when present, because a leftover is the
# difference between one Bangla keyboard in the picker and two.
$oldClsid = '{35F67E9D-A54D-4177-9697-8B0AB71A9E04}'
$oldTip = "0845:$oldClsid{9B4E7C21-3D5A-4F86-A2E1-6C0D8B7F5A13}"

# The Desktop, so a tester finds it without being told what LOCALAPPDATA is.
#
# Not the install directory: that is under Program Files, which a standard user cannot write to.
# PowerShell's manifest disables UAC file virtualization, so such a write fails outright rather than
# being silently redirected, and this script is meant to run without admin.
$dataDir = Join-Path $(if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { $env:USERPROFILE }) 'Likhi'
$reportName = "Likhi-diagnostics-$env:COMPUTERNAME-$(Get-Date -Format yyyyMMdd-HHmmss).txt"
$report = $null
foreach ($dir in @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('MyDocuments'), $env:TEMP)) {
    if (-not $dir) { continue }
    try {
        if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force -ErrorAction Stop | Out-Null }
        $report = Join-Path $dir $reportName
        break
    } catch { }
}
$transcribing = $false
try { Start-Transcript -Path $report -Force -ErrorAction Stop | Out-Null; $transcribing = $true } catch { }

function Section($name) { Write-Host "`n== $name" }

Write-Host "Likhi diagnostics  $(Get-Date -Format s)"
Write-Host "Windows $([Environment]::OSVersion.Version)  user=$env:USERNAME  admin=$(([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))"

Section "1. Installed files"
$root = "$env:ProgramFiles\Likhi"
if (Test-Path $root) {
    Write-Host "  FOUND $root"
    foreach ($f in @("shell\x64\LikhiTextService.dll", "shell\x86\LikhiTextService.dll",
                     "shell\x64\likhi.ico", "config.json", "Likhi.exe",
                     "engine\likhi-server.exe", "engine\models\lexicon\unigrams.lkx",
                     "engine\models\indicxlit\model.lkw")) {
        $p = Join-Path $root $f
        if (Test-Path $p) { Write-Host "    ok      $f" } else { Write-Host "    MISSING $f" }
    }
} else { Write-Host "  MISSING $root" }
# A leftover PIME install is not an error -- someone may use it for another language -- but our
# files inside it are, because they mean an upgrade did not finish cleaning up.
$pime = "${env:ProgramFiles(x86)}\PIME"
if (Test-Path $pime) {
    Write-Host "  note: PIME present at $pime"
    if (Test-Path (Join-Path $pime 'python\input_methods\likhi')) {
        Write-Host "    LEFTOVER our old input method is still in there"
    }
}

Section "2. COM registration (created by regsvr32)"
foreach ($k in "HKLM:\SOFTWARE\Classes\CLSID\$clsid\InprocServer32",
               "HKLM:\SOFTWARE\Classes\WOW6432Node\CLSID\$clsid\InprocServer32") {
    if (Test-Path $k) { Write-Host "  ok      $((Get-ItemProperty $k).'(default)')" }
    else { Write-Host "  MISSING $k" }
}

Section "3. Text service language profile"
$lp = "HKLM:\SOFTWARE\Microsoft\CTF\TIP\$clsid\LanguageProfile"
if (Test-Path $lp) {
    $found = $false
    Get-ChildItem $lp | ForEach-Object {
        $lang = $_.PSChildName
        Get-ChildItem $_.PSPath -ErrorAction SilentlyContinue | ForEach-Object {
            $desc = (Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue).Description
            Write-Host "  $lang / $($_.PSChildName)  '$desc'"
            if ($_.PSChildName -eq $profileGuid) { $script:found = $true }
        }
    }
    if (-not $found) { Write-Host "  MISSING the Likhi profile $profileGuid" }
} else { Write-Host "  MISSING $lp  (regsvr32 did not register any profile)" }

Section "4. This user's keyboards"
# Indexed, not piped: Get-WinUserLanguageList hands back the List as one object, so piping it
# member-enumerates and prints every language on a single line, which is unreadable in a report.
$langs = Get-WinUserLanguageList
$hasTip = $false
for ($i = 0; $i -lt $langs.Count; $i++) {
    Write-Host "  $($langs[$i].LanguageTag): $($langs[$i].InputMethodTips -join ', ')"
    if ($langs[$i].InputMethodTips -contains $tip) { $hasTip = $true }
}
Write-Host "  Likhi present for this user: $hasTip"
$hasOld = $false
for ($i = 0; $i -lt $langs.Count; $i++) { if ($langs[$i].InputMethodTips -contains $oldTip) { $hasOld = $true } }
if ($hasOld) { Write-Host "  LEFTOVER the pre-0.2.0 keyboard is also listed; there will be two Bangla entries" }
try { Write-Host "  default input method: $((Get-WinDefaultInputMethodOverride).InputMethodTip)" } catch {}

Section "5. Processes"
# Only the engine now: the text service is a DLL Windows loads into each application itself, so
# there is no launcher and no separate backend process to look for.
# likhi-server is the engine since 0.3; pythonw is what an older install left behind, and it is
# worth reporting because it would still be holding the port.
foreach ($n in 'likhi-server', 'pythonw', 'python') {
    $procs = Get-Process $n -ErrorAction SilentlyContinue
    if ($procs) { $procs | ForEach-Object { Write-Host "  $n  pid=$($_.Id)  $($_.Path)" } }
    else { Write-Host "  $n not running" }
}
$loaded = @()
Get-Process -ErrorAction SilentlyContinue | ForEach-Object {
    try { if ($_.Modules | Where-Object { $_.ModuleName -eq 'LikhiTextService.dll' }) { $loaded += "$($_.ProcessName)($($_.Id))" } } catch {}
}
Write-Host "  text service loaded in: $(if ($loaded) { $loaded -join ', ' } else { 'no application yet' })"

Section "6. Engine"
try {
    $c = New-Object Net.Sockets.TcpClient('127.0.0.1', 47123)
    $s = $c.GetStream(); $w = New-Object IO.StreamWriter($s); $r = New-Object IO.StreamReader($s)
    $w.WriteLine('{"op":"ping"}'); $w.Flush()
    Write-Host "  engine replied: $($r.ReadLine())"
    $c.Close()
} catch { Write-Host "  engine NOT responding on 127.0.0.1:47123" }

# Inlined rather than left as a second file to ask for: when the engine fails to start, the reason
# is the last few lines of server.err and nothing in the registry says anything about it.
foreach ($name in 'server.err', 'server.log') {
    $p = Join-Path $dataDir $name
    if (Test-Path $p) {
        $size = (Get-Item $p).Length
        Write-Host "  --- $name ($size bytes, last 20 lines)"
        Get-Content $p -Tail 20 -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "      $_" }
    } else { Write-Host "  --- $name absent" }
}
$idFile = Join-Path $dataDir 'install_id'
if (Test-Path $idFile) { Write-Host "  install id: $((Get-Content $idFile -Raw).Trim())" }
else { Write-Host "  install id: none (the engine has never run for this user)" }

Section "7. Autostart"
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$v = (Get-ItemProperty $run -Name 'LikhiEngine' -ErrorAction SilentlyContinue).LikhiEngine
if ($v) { Write-Host "  ok      LikhiEngine = $v" } else { Write-Host "  MISSING LikhiEngine" }
$old = (Get-ItemProperty $run -Name 'LikhiLauncher' -ErrorAction SilentlyContinue).LikhiLauncher
if ($old) { Write-Host "  LEFTOVER LikhiLauncher = $old  (there is no launcher any more)" }

Section "8. Recent crashes involving text input"
# A text service DLL runs inside every application that has keyboard focus, so a bad one shows up
# as other programs dying in MSCTF.dll rather than as anything named Likhi.
# Get-WinEvent throws rather than returning nothing when no event matches, so a machine with a clean
# log used to print "could not read the Application log", which reads like a fault and is the exact
# opposite of what it means. SilentlyContinue, then decide from the result.
try {
    $errs = Get-WinEvent -FilterHashtable @{LogName='Application'; ProviderName='Application Error'; StartTime=(Get-Date).AddDays(-7)} -ErrorAction SilentlyContinue
    $hits = $errs | Where-Object { $_.Message -match 'MSCTF|PIMETextService|ctfmon' }
    if ($hits) {
        $hits | Select-Object -First 15 | ForEach-Object {
            $lines = $_.Message -split "`r?`n"
            $app = (($lines | Where-Object { $_ -match 'Faulting application name' }) -replace '.*name: ','' -replace ',.*','')
            $mod = (($lines | Where-Object { $_ -match 'Faulting module name' }) -replace '.*name: ','' -replace ',.*','')
            $code = (($lines | Where-Object { $_ -match 'Exception code' }) -replace '.*code: ','')
            Write-Host "  $($_.TimeCreated)  $app  in $mod  $code"
        }
    } else { Write-Host "  none in the last 7 days" }
} catch { Write-Host "  could not read the Application log: $($_.Exception.Message)" }

Section "9. Pending restart"
# Setup refuses to run while Windows has a file rename queued for our files, reporting only that
# "the installation/removal of a previous program was not completed", which tells nobody what to do.
# The answer is always: restart, then run Setup again.
$pfro = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager' -Name PendingFileRenameOperations -ErrorAction SilentlyContinue).PendingFileRenameOperations
$ours = @($pfro | Where-Object { $_ -match 'PIME|Likhi' })
if ($ours.Count -gt 0) {
    Write-Host "  RESTART NEEDED before installing again. Queued for the next boot:"
    $ours | ForEach-Object { Write-Host "    $_" }
} elseif ($pfro) {
    Write-Host "  none of ours ($(@($pfro | Where-Object { $_ }).Count) unrelated entries queued by other software)"
} else {
    Write-Host "  nothing queued"
}

Section "10. Installer logs"
# The copy under the install directory is the one that survives; %TEMP% is cleaned by Windows, and
# when Setup was elevated with a different admin account its log is in that account's TEMP, not this
# user's. Send Setup.log alongside this report.
$setupLogs = @()
foreach ($p in @("$env:ProgramFiles\Likhi\Setup.log",
                 "${env:ProgramFiles(x86)}\Likhi\Setup.log",
                 (Join-Path ([Environment]::GetFolderPath('Desktop')) 'Likhi-setup-log.txt'))) {
    if (Test-Path $p) { $setupLogs += Get-Item $p }
}
$setupLogs += Get-ChildItem "$env:TEMP\Setup Log*.txt" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 3
if ($setupLogs) { $setupLogs | ForEach-Object { Write-Host "  $($_.LastWriteTime)  $($_.FullName)" } }
else { Write-Host "  none found (a pre-0.1.3 install did not keep its log)" }

if ($transcribing) {
    try { Stop-Transcript | Out-Null } catch { }
    # Keep the three most recent. This lands on someone's Desktop, so leaving a growing pile of
    # reports there is a good way to have the tool resented.
    try {
        Get-ChildItem (Split-Path $report) -Filter 'Likhi-diagnostics-*.txt' -ErrorAction Stop |
            Sort-Object LastWriteTime -Descending | Select-Object -Skip 3 |
            Remove-Item -Force -ErrorAction SilentlyContinue
    } catch { }
    Write-Host ""
    Write-Host "Saved to: $report"
    Write-Host "Send that file back."
    # Open the folder with the report selected, so nobody has to know where LOCALAPPDATA is.
    try { Start-Process explorer.exe "/select,`"$report`"" } catch { }
} else {
    Write-Host "`nCould not write a file; copy everything above and send it back."
}
