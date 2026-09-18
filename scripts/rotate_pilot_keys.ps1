# Rotate the pilot telemetry keys on Cloud Run.
#
#     powershell -File scripts\rotate_pilot_keys.ps1                 # rotate both
#     powershell -File scripts\rotate_pilot_keys.ps1 -Only admin     # rotate just the admin key
#     powershell -File scripts\rotate_pilot_keys.ps1 -WhatIf         # print what it would do
#
# There are two keys and they are not the same kind of secret:
#
#   ingest key   Baked into every installed client so it can POST its own telemetry. Append only:
#                holding it lets someone send junk, not read anything. It cannot be kept secret --
#                anyone who installs Likhi can read it out of config.json -- so rotating it is
#                housekeeping, and it invalidates every client already installed until they are
#                given a new build.
#
#   admin key    Grants GET /v1/export, which returns everything every install has ever reported.
#                It is never shipped. This is the one that matters: with it, a stranger reads your
#                testers' struggle words. It was pasted into a chat transcript, which is why it is
#                being rotated -- not because anything is known to have gone wrong, but because a
#                secret that has been copied out of its container is no longer a secret, and this
#                costs one command.
#
# Afterwards pilot.local.json holds the new values, and rebuilding the client with
# scripts/build_client.py stamps the new ingest key into installers.

[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [ValidateSet("both", "ingest", "admin")]
    [string]$Only = "both",
    [string]$Service = "likhi-ingest",
    [string]$Region  = "us-central1",
    [string]$Project = "khaledbinamir",
    [string]$Secrets = "$PSScriptRoot\..\pilot.local.json"
)

$ErrorActionPreference = "Stop"

function New-Key {
    # 32 bytes of cryptographic randomness, url-safe. Long enough that guessing is not a threat
    # model, short enough to paste.
    $bytes = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    [Convert]::ToBase64String($bytes).Replace('+', '-').Replace('/', '_').TrimEnd('=')
}

if (-not (Get-Command gcloud -ErrorAction SilentlyContinue)) {
    # Not installed here as of 2026-09-18. Either install it, or do the same thing in the browser:
    # the two are equivalent, and the console needs nothing installed.
    Write-Host "gcloud is not on PATH. Two ways forward:"
    Write-Host ""
    Write-Host "  1. Install it:  winget install Google.CloudSDK   then  gcloud auth login"
    Write-Host "     and run this script again."
    Write-Host ""
    Write-Host "  2. Do it in the browser, which needs nothing installed:"
    Write-Host "     https://console.cloud.google.com/run/detail/$Region/$Service/revisions?project=$Project"
    Write-Host "     Edit & Deploy New Revision -> Variables & Secrets, replace the values of"
    Write-Host "     LIKHI_INGEST_KEY and LIKHI_ADMIN_KEY, then Deploy."
    Write-Host ""
    Write-Host "     Two freshly generated keys to paste, if useful:"
    Write-Host ("       LIKHI_INGEST_KEY = " + (New-Key))
    Write-Host ("       LIKHI_ADMIN_KEY  = " + (New-Key))
    Write-Host ""
    Write-Host "     Then put the same values into pilot.local.json (git-ignored) so likhi-report"
    Write-Host "     can still reach the export."
    exit 1
}

# Read the current file so keys that are not being rotated are preserved exactly.
$current = @{}
if (Test-Path $Secrets) {
    $json = Get-Content $Secrets -Raw -Encoding UTF8
    ($json | ConvertFrom-Json).PSObject.Properties | ForEach-Object { $current[$_.Name] = $_.Value }
}

$new = @{}
if ($Only -in @("both", "ingest")) { $new["ingest_key"] = New-Key }
if ($Only -in @("both", "admin"))  { $new["admin_key"]  = New-Key }

Write-Host "Rotating on $Service ($Region, project $Project):"
$new.Keys | Sort-Object | ForEach-Object { Write-Host "  $_" }

# Both keys are set in one update so the service restarts once. Setting them separately would
# leave a window where the new ingest key is live but the admin key is not, and every client
# would be rejected in between.
$envPairs = @()
if ($new.ContainsKey("ingest_key")) { $envPairs += "LIKHI_INGEST_KEY=$($new['ingest_key'])" }
if ($new.ContainsKey("admin_key"))  { $envPairs += "LIKHI_ADMIN_KEY=$($new['admin_key'])" }
$envArg = $envPairs -join ","

if ($PSCmdlet.ShouldProcess("$Service in $Region", "update environment variables")) {
    & gcloud run services update $Service `
        --region $Region `
        --project $Project `
        --update-env-vars $envArg `
        --quiet
    if ($LASTEXITCODE -ne 0) { throw "gcloud returned $LASTEXITCODE; the keys on disk were not changed" }

    # Only written after the service accepted them, so a failed deploy never leaves the file
    # claiming a key the server does not honour.
    foreach ($k in $new.Keys) { $current[$k] = $new[$k] }
    if (-not $current.ContainsKey("endpoint")) {
        $current["endpoint"] = "https://likhi-ingest-924534241684.$Region.run.app/v1/ingest"
    }
    $current["rotated"] = (Get-Date).ToString("yyyy-MM-dd")

    # UTF8 without a byte-order mark: the readers of this file strip one, but nothing else should
    # have to. Windows PowerShell 5.1 has no null-coalescing operator, hence the explicit test.
    $target = $Secrets
    $resolved = Resolve-Path $Secrets -ErrorAction SilentlyContinue
    if ($resolved) { $target = $resolved.Path }
    $out = ($current | ConvertTo-Json -Depth 4)
    [System.IO.File]::WriteAllText($target, $out, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host ""
    Write-Host "Wrote $Secrets (git-ignored)."
}

Write-Host ""
Write-Host "Verify:"
Write-Host "  gcloud run services describe $Service --region $Region --project $Project --format='value(spec.template.spec.containers[0].env)'"
if ($new.ContainsKey("admin_key")) {
    Write-Host "  engine\target\release\likhi-report.exe pull --out pilot     # should still work"
}
if ($new.ContainsKey("ingest_key")) {
    Write-Host ""
    Write-Host "The ingest key changed, so every already-installed client is now rejected."
    Write-Host "Rebuild and redistribute:  uv run python scripts\build_client.py"
}
