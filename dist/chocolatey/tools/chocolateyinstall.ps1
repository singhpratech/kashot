$ErrorActionPreference = 'Stop'

$packageName = 'kashot'
$url64       = 'https://github.com/singhpratech/kashot/releases/download/v0.5.0/Kashot.msi'
$checksum64  = '8bfcbb53fc7a67c3ec5de08859122f21bab733427d25c2930c1676723300d5a6'

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
