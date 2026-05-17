# KAShot installer — Windows PowerShell.
#
# Quick install (latest release):
#   iwr -useb https://kashot.org/install.ps1 | iex
#
# Pin a specific version:
#   & ([scriptblock]::Create((iwr -useb https://kashot.org/install.ps1))) -Tag v0.3.0
#
# Pick a custom install dir:
#   & ([scriptblock]::Create((iwr -useb https://kashot.org/install.ps1))) -InstallDir 'C:\Tools\Kashot'
#
# Defaults to %LOCALAPPDATA%\Programs\Kashot — user-scope, no admin required.

[CmdletBinding()]
param(
    [string]$Tag        = '',
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\Kashot')
)

$ErrorActionPreference = 'Stop'
$ProgressPreference    = 'SilentlyContinue'

$Owner = 'singhpratech'
$Repo  = 'kashot'

Write-Host '-> KAShot installer (Windows)'
Write-Host ''

# ── Resolve tag ───────────────────────────────────────────────────────────────
try {
    $release = Invoke-RestMethod `
        -Uri  "https://api.github.com/repos/$Owner/$Repo/releases/$(if ($Tag) { "tags/$Tag" } else { 'latest' })" `
        -Headers @{ 'Accept' = 'application/vnd.github+json'; 'User-Agent' = 'kashot-installer' }
} catch {
    Write-Error 'kashot: could not reach github.com/api (rate-limited or offline?)'
    exit 1
}

$Tag = $release.tag_name
$asset = $release.assets | Where-Object { $_.name -match 'windows.*x86_64\.zip$' } | Select-Object -First 1

if (-not $asset) {
    Write-Error "kashot: no Windows artifact in release $Tag (expected kashot-windows-x86_64.zip)"
    exit 1
}

Write-Host "   version:    $Tag"
Write-Host "   artifact:   $($asset.name)"
Write-Host "   source:     $($asset.browser_download_url)"
Write-Host "   install:    $InstallDir\kashot.exe"
Write-Host ''

# ── Stop & clean any existing install ─────────────────────────────────────────
# Kill the running kashot.exe (if any) so the new file can replace the old
# one — Windows will refuse to overwrite a locked exe. Then remove the old
# binary at the target path so the install never partially overwrites.
$running = Get-Process -Name kashot -ErrorAction SilentlyContinue
if ($running) {
    Write-Host '   stopping running kashot.exe...'
    $running | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 600
}

$existing = Join-Path $InstallDir 'kashot.exe'
if (Test-Path $existing) {
    Write-Host "   removing previous binary at $existing"
    Remove-Item $existing -Force -ErrorAction SilentlyContinue
}

# Warn if another kashot.exe is reachable on PATH from a different dir.
$onPath = (Get-Command kashot -ErrorAction SilentlyContinue).Source
if ($onPath -and $onPath -ne $existing) {
    Write-Host "   heads up: another kashot.exe is on your PATH at $onPath"
    Write-Host "     remove it with: Remove-Item '$onPath'"
}

# ── Download + extract ────────────────────────────────────────────────────────
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "kashot-$([guid]::NewGuid())"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$zip = Join-Path $tmp $asset.name

try {
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip -UseBasicParsing
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Expand-Archive -Path $zip -DestinationPath $InstallDir -Force
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

$exe = Join-Path $InstallDir 'kashot.exe'
if (-not (Test-Path $exe)) {
    # Some zip layouts include a top-level kashot/ folder.
    $found = Get-ChildItem -Path $InstallDir -Filter 'kashot.exe' -Recurse | Select-Object -First 1
    if ($found) {
        Move-Item $found.FullName $exe -Force
    } else {
        Write-Error 'kashot: kashot.exe missing from the extracted archive.'
        exit 1
    }
}

# ── Add to user PATH (so `kashot` works in new terminals) ────────────────────
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not ($userPath -split ';' | Where-Object { $_ -eq $InstallDir })) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$InstallDir", 'User')
    Write-Host "   added to user PATH (open a new terminal to pick it up)"
}

Write-Host ''
Write-Host "[ok] kashot installed -> $exe"
Write-Host ''
Write-Host '   run:        kashot'
Write-Host "   uninstall:  Remove-Item -Recurse -Force '$InstallDir'"
Write-Host '   docs:       https://kashot.org'
