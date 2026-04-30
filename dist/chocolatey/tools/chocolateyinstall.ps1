$ErrorActionPreference = 'Stop'

$packageName = 'kashot'
$url64       = 'https://github.com/singhpratech/kashot/releases/download/v0.1.0/Kashot.msi'
$checksum64  = 'REPLACE_WITH_ACTUAL_SHA256_AT_RELEASE_TIME'

$packageArgs = @{
  packageName    = $packageName
  fileType       = 'msi'
  url64bit       = $url64
  checksum64     = $checksum64
  checksumType64 = 'sha256'
  silentArgs     = '/quiet /norestart'
  validExitCodes = @(0, 3010, 1641)
}

Install-ChocolateyPackage @packageArgs
