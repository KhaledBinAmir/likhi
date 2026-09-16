<#
.SYNOPSIS
  Report why the Likhi keyboard is or is not working on this machine.

  Run on any machine, no admin needed, and send the output back. It reads state only and changes
  nothing.

    powershell -NoProfile -ExecutionPolicy Bypass -File diagnose.ps1
#>

$clsid = '{35F67E9D-A54D-4177-9697-8B0AB71A9E04}'
$profileGuid = '{9B4E7C21-3D5A-4F86-A2E1-6C0D8B7F5A13}'
$tip = "0845:$clsid$profileGuid"

function Section($name) { Write-Host "`n== $name" }

Write-Host "Likhi diagnostics  $(Get-Date -Format s)"
Write-Host "Windows $([Environment]::OSVersion.Version)  user=$env:USERNAME  admin=$(([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))"

Section "1. Installed files"
$roots = @("$env:ProgramFiles\Likhi", "${env:ProgramFiles(x86)}\Likhi", "${env:ProgramFiles(x86)}\PIME")
foreach ($r in $roots) {
    if (Test-Path $r) {
        Write-Host "  FOUND $r"
        foreach ($f in @("pime\x64\PIMETextService.dll", "pime\x86\PIMETextService.dll",
                         "pime\PIMELauncher.exe", "pime\backends.json",
                         "pime\python\input_methods\likhi\ime.json",
                         "pime\python\input_methods\likhi\config.json",
                         "runtime\likhi-server.cmd", "runtime\models\lexicon\unigrams.marisa",
                         "x64\PIMETextService.dll", "python\input_methods\likhi\ime.json")) {
            $p = Join-Path $r $f
            if (Test-Path $p) { Write-Host "    ok      $f" }
        }
    } else { Write-Host "  absent $r" }
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
Get-WinUserLanguageList | ForEach-Object { Write-Host "  $($_.LanguageTag): $($_.InputMethodTips -join ', ')" }
$hasTip = (Get-WinUserLanguageList | ForEach-Object { $_.InputMethodTips }) -contains $tip
Write-Host "  Likhi present for this user: $hasTip"
try { Write-Host "  default input method: $((Get-WinDefaultInputMethodOverride).InputMethodTip)" } catch {}

Section "5. Processes"
foreach ($n in 'PIMELauncher', 'pythonw', 'python') {
    $procs = Get-Process $n -ErrorAction SilentlyContinue
    if ($procs) { $procs | ForEach-Object { Write-Host "  $n  pid=$($_.Id)  $($_.Path)" } }
    else { Write-Host "  $n not running" }
}

Section "6. Engine"
try {
    $c = New-Object Net.Sockets.TcpClient('127.0.0.1', 47123)
    $s = $c.GetStream(); $w = New-Object IO.StreamWriter($s); $r = New-Object IO.StreamReader($s)
    $w.WriteLine('{"op":"ping"}'); $w.Flush()
    Write-Host "  engine replied: $($r.ReadLine())"
    $c.Close()
} catch { Write-Host "  engine NOT responding on 127.0.0.1:47123" }

Section "7. Autostart"
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
foreach ($n in 'LikhiLauncher', 'LikhiEngine') {
    $v = (Get-ItemProperty $run -Name $n -ErrorAction SilentlyContinue).$n
    if ($v) { Write-Host "  ok      $n = $v" } else { Write-Host "  MISSING $n" }
}

Write-Host "`nSend everything above back."
