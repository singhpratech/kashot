$ErrorActionPreference = 'Stop'

$packageArgs = @{
  packageName    = 'kashot'
  fileType       = 'msi'
  silentArgs     = '/quiet /norestart'
  validExitCodes = @(0, 3010, 1605, 1614, 1641)
  file           = ''  # MSI uninstall via product code
  softwareName   = 'Kashot*'
}

# Installed product code matches the WiX UpgradeCode in Installer/Kashot.wxs
$key = Get-UninstallRegistryKey -SoftwareName $packageArgs.softwareName
if ($key.Count -eq 1) {
  $packageArgs['silentArgs'] = "$($key[0].PSChildName) /quiet /norestart"
  $packageArgs['file']       = ''
  Uninstall-ChocolateyPackage @packageArgs
} elseif ($key.Count -eq 0) {
  Write-Warning "$($packageArgs.packageName) is not installed."
} else {
  Write-Warning "$($key.Count) matches found — please uninstall manually."
}
