# Build Kashot.msi
# Run from repo root or from this folder; it locates the published exe automatically.

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$kashotProj = Join-Path $repoRoot 'Kashot\Kashot.csproj'
$publishDir = Join-Path $repoRoot 'Kashot\bin\Release\net8.0-windows\win-x64\publish'
$wxs        = Join-Path $PSScriptRoot 'Kashot.wxs'
$out        = Join-Path $repoRoot 'Kashot.msi'

if (-not (Test-Path (Join-Path $publishDir 'Kashot.exe'))) {
    Write-Host "Publishing Kashot first..."
    dotnet publish $kashotProj -c Release -r win-x64 --self-contained true `
        -p:PublishSingleFile=true `
        -p:IncludeNativeLibrariesForSelfExtract=true `
        -p:EnableCompressionInSingleFile=true | Out-Null
}

# Copy the icon next to the exe so WiX bindpath finds both via the same root
Copy-Item -Path (Join-Path $repoRoot 'Kashot\Kashot.ico') -Destination $publishDir -Force

Write-Host "Building MSI..."
wix build $wxs -arch x64 -bindpath "app=$publishDir\" -out $out

if (Test-Path $out) {
    $sz = [math]::Round((Get-Item $out).Length / 1MB, 1)
    Write-Host "OK: $out ($sz MB)"
} else {
    throw "MSI build failed"
}
