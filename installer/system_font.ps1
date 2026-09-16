<#
.SYNOPSIS
  Make Bangla render in a chosen font everywhere on this machine, and put it back.

    system_font.ps1 -Apply "Noto Sans Bengali"
    system_font.ps1 -Restore
    system_font.ps1 -Status

  Needs administrator: the setting is machine-wide.

.DESCRIPTION
  Windows has no "Bangla font" to change. Applications ask for a font like Segoe UI, that font has
  no Bengali glyphs, and Windows follows a per-font fallback chain -- font linking -- to find one
  that does. On a normal machine that chain ends at Nirmala UI, which is why all Bangla looks the
  same everywhere. Putting another face at the front of those chains changes what Bangla looks like
  in applications that never named a Bengali font, which is nearly all of them.

  What this deliberately does NOT do is substitute Nirmala UI itself. Nirmala covers about ten
  Indic scripts -- Devanagari, Tamil, Telugu, Gujarati, Kannada, Malayalam, Odia, Gurmukhi, Sinhala
  -- and redirecting it to a Bengali-only face would break every one of them. Font linking is
  per-character: a font is consulted only for characters it actually has, so adding a Bengali face
  to the front of a chain cannot affect any other script.

  Every original value is written to a backup file before anything changes, and -Restore puts back
  exactly what was there, including removing keys that did not exist before. Nothing is guessed.

  Applications read their fallback chain when they start, so most need restarting to pick this up,
  and Explorer needs a restart for the shell itself.
#>
[CmdletBinding(DefaultParameterSetName = 'Status')]
param(
    [Parameter(ParameterSetName = 'Apply', Mandatory = $true)]
    [string]$Apply,
    [Parameter(ParameterSetName = 'Restore', Mandatory = $true)]
    [switch]$Restore,
    [Parameter(ParameterSetName = 'Status')]
    [switch]$Status
)

$ErrorActionPreference = 'Stop'

$LinkKey = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\FontLink\SystemLink'
$BackupPath = Join-Path $env:ProgramData 'Likhi\system-font-backup.json'

# The fonts applications actually ask for. Each gets our face at the front of its fallback chain.
$TargetFonts = @(
    'Segoe UI', 'Tahoma', 'Microsoft Sans Serif', 'Arial', 'Calibri',
    'Times New Roman', 'Courier New', 'Verdana', 'Consolas', 'Segoe UI Variable'
)

function Assert-Admin {
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw "This changes a machine-wide setting and needs to run as administrator."
    }
}

function Get-FontFile([string]$family) {
    # The file behind an installed family, machine-wide first then per-user, as the registry names
    # it. Font linking needs the file name, not just the family.
    foreach ($root in @('HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts',
                        'HKCU:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts')) {
        $key = Get-Item $root -ErrorAction SilentlyContinue
        if (-not $key) { continue }
        foreach ($name in $key.Property) {
            if ($name -match "^$([regex]::Escape($family))( \(TrueType\)| \(OpenType\))?$") {
                $value = (Get-ItemProperty $root -Name $name).$name
                return [IO.Path]::GetFileName($value)
            }
        }
    }
    return $null
}

function Show-Status {
    Write-Host "backup: $(if (Test-Path $BackupPath) { $BackupPath } else { 'none -- nothing has been changed' })"
    foreach ($f in $TargetFonts) {
        $chain = (Get-ItemProperty $LinkKey -Name $f -ErrorAction SilentlyContinue).$f
        if ($chain) { Write-Host ("  {0,-22} {1}" -f $f, ($chain -join ' ; ')) }
        else { Write-Host ("  {0,-22} (no chain)" -f $f) }
    }
}

if ($PSCmdlet.ParameterSetName -eq 'Status' -or $Status) { Show-Status; exit 0 }

Assert-Admin

if ($Restore) {
    if (-not (Test-Path $BackupPath)) { throw "No backup at $BackupPath; nothing to restore." }
    $backup = Get-Content $BackupPath -Raw | ConvertFrom-Json
    foreach ($entry in $backup.entries) {
        if ($null -eq $entry.value) {
            # The chain did not exist before we added one: remove it rather than leaving an empty.
            Remove-ItemProperty -Path $LinkKey -Name $entry.font -ErrorAction SilentlyContinue
            Write-Host "removed $($entry.font) (it had no chain before)"
        } else {
            Set-ItemProperty -Path $LinkKey -Name $entry.font -Value ([string[]]$entry.value) -Type MultiString
            Write-Host "restored $($entry.font)"
        }
    }
    Remove-Item $BackupPath -Force
    Write-Host "`nDone. Restart applications, or sign out and in, to see the change."
    return
}

# ---------------------------------------------------------------- apply

$file = Get-FontFile $Apply
if (-not $file) { throw "'$Apply' is not installed on this machine." }
Write-Host "using $Apply ($file)"

if (-not (Test-Path $LinkKey)) { New-Item -Path $LinkKey -Force | Out-Null }

# Back up first, and only once: applying twice must not overwrite the original values with our own.
if (-not (Test-Path $BackupPath)) {
    New-Item -ItemType Directory -Path (Split-Path $BackupPath) -Force | Out-Null
    $entries = foreach ($f in $TargetFonts) {
        $existing = (Get-ItemProperty $LinkKey -Name $f -ErrorAction SilentlyContinue).$f
        [pscustomobject]@{ font = $f; value = $existing }
    }
    @{ taken = (Get-Date).ToString('s'); entries = $entries } | ConvertTo-Json -Depth 5 |
        Set-Content $BackupPath -Encoding utf8
    Write-Host "backed up the original chains to $BackupPath"
} else {
    Write-Host "keeping the existing backup at $BackupPath"
}

$entry = "$file,$Apply"
foreach ($f in $TargetFonts) {
    $chain = @((Get-ItemProperty $LinkKey -Name $f -ErrorAction SilentlyContinue).$f)
    $chain = @($chain | Where-Object { $_ -and $_ -notlike "*,$Apply" })   # drop our own earlier entry
    Set-ItemProperty -Path $LinkKey -Name $f -Value ([string[]](@($entry) + $chain)) -Type MultiString
    Write-Host "  $f -> $Apply first"
}

Write-Host "`nDone. Applications read their fallback chain when they start, so restart them."
Write-Host "To put everything back exactly as it was:  system_font.ps1 -Restore"
