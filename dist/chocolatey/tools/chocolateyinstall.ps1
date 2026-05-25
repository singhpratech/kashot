$ErrorActionPreference = 'Stop'

$packageName = 'kashot'
$url64       = 'https://github.com/singhpratech/kashot/releases/download/v0.4.2/Kashot.msi'
$checksum64  = 'f7833bee9f2dab5e7382107c4300ff8a59ae4ae72380613d329e0e72c3f7faae'

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
