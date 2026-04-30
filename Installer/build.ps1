# Build all Windows distribution artifacts in one go.
#
# Outputs at repo root:
#   Kashot.msi              - signed-when-cert-available installer (WiX 4)
#   Kashot.exe              - self-contained single-file binary (no .NET install needed)
#   Kashot-portable.zip     - zipped publish folder; extract anywhere and run
#
# Run from repo root or from this folder; paths resolve from $PSScriptRoot.
#
# Requirements:
#   - dotnet 8 SDK
#   - wix 4 (`dotnet tool install --global wix`)

$ErrorActionPreference = 'Stop'

$repoRoot   = Split-Path -Parent $PSScriptRoot
$proj       = Join-Path $repoRoot 'Kashot\Kashot.csproj'
$publishDir = Join-Path $repoRoot 'Kashot\bin\Release\net8.0-windows\win-x64\publish'
$wxs        = Join-Path $PSScriptRoot 'Kashot.wxs'

$outMsi     = Join-Path $repoRoot 'Kashot.msi'
$outExe     = Join-Path $repoRoot 'Kashot.exe'
$outZip     = Join-Path $repoRoot 'Kashot-portable.zip'

# ---- 1. Publish single-file self-contained exe -----------------------------

Write-Host "Publishing Kashot (single-file, self-contained, win-x64)..." -ForegroundColor Cyan
dotnet publish $proj -c Release -r win-x64 --self-contained true `
    -p:PublishSingleFile=true `
    -p:IncludeNativeLibrariesForSelfExtract=true `
    -p:EnableCompressionInSingleFile=true `
    -p:DebugType=embedded `
    -p:GenerateDocumentationFile=false | Out-Null

if (-not (Test-Path (Join-Path $publishDir 'Kashot.exe'))) {
    throw "Publish failed: Kashot.exe not found in $publishDir"
}

# Copy the icon next to the exe so WiX bindpath finds both via the same root
Copy-Item -Path (Join-Path $repoRoot 'Kashot\Kashot.ico') -Destination $publishDir -Force

# ---- 2. Standalone EXE at repo root ----------------------------------------

Write-Host "Copying standalone Kashot.exe to repo root..." -ForegroundColor Cyan
Copy-Item -Path (Join-Path $publishDir 'Kashot.exe') -Destination $outExe -Force

# ---- 3. Portable ZIP -------------------------------------------------------

Write-Host "Building Kashot-portable.zip..." -ForegroundColor Cyan
if (Test-Path $outZip) { Remove-Item $outZip -Force }

# Zip a temp dir laid out as Kashot/Kashot.exe so it extracts cleanly
$stage = Join-Path $env:TEMP "Kashot-portable-$([Guid]::NewGuid().ToString('N').Substring(0,8))"
$stageInner = Join-Path $stage 'Kashot'
New-Item -ItemType Directory -Path $stageInner -Force | Out-Null
Copy-Item -Path (Join-Path $publishDir 'Kashot.exe') -Destination $stageInner -Force
Copy-Item -Path (Join-Path $repoRoot 'Kashot\Kashot.ico') -Destination $stageInner -Force
Compress-Archive -Path (Join-Path $stage 'Kashot') -DestinationPath $outZip -CompressionLevel Optimal
Remove-Item $stage -Recurse -Force

# ---- 4. MSI ----------------------------------------------------------------

Write-Host "Building Kashot.msi (WiX)..." -ForegroundColor Cyan
wix build $wxs -arch x64 -bindpath "app=$publishDir\" -out $outMsi

# ---- 5. Summary ------------------------------------------------------------

function Show-Artifact($path) {
    if (Test-Path $path) {
        $sz = [math]::Round((Get-Item $path).Length / 1MB, 1)
        $hash = (Get-FileHash -Algorithm SHA256 $path).Hash
        Write-Host ("  {0,-28}  {1,6} MB  sha256={2}" -f (Split-Path -Leaf $path), $sz, $hash) -ForegroundColor Green
    } else {
        Write-Warning "missing: $path"
    }
}

Write-Host ""
Write-Host "Artifacts at repo root:" -ForegroundColor Yellow
Show-Artifact $outMsi
Show-Artifact $outExe
Show-Artifact $outZip

Write-Host ""
Write-Host "Next steps:" -ForegroundColor Yellow
Write-Host "  - Tag release in GitHub UI (e.g. v0.1)"
Write-Host "  - Attach all three files: Kashot.msi, Kashot.exe, Kashot-portable.zip"
Write-Host "  - Publish; the kashot.org download buttons resolve immediately."
