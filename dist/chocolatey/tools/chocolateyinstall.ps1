$ErrorActionPreference = 'Stop'

$packageName = 'kashot'
$url64       = 'https://github.com/singhpratech/kashot/releases/download/v0.4.3/Kashot.msi'
$checksum64  = 'f2ecb0cc1d6d222dcfe0370b2947ad332bfe3121fc5e712cd748522066f0194a'

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
