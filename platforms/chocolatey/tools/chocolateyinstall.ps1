$ErrorActionPreference = 'Stop'
$packageName = 'oplirex'
$version = '1.0.0'
$url64 = "https://github.com/nxyystore/oplirex/releases/download/v$version/oplirex-windows-x86_64.zip"
$checksum64 = 'REPLACEME_SHA256'
$toolsDir = "$(Split-Path -Parent $MyInvocation.MyCommand.Definition)"

$packageArgs = @{
  packageName    = $packageName
  url64bit       = $url64
  checksum64     = $checksum64
  checksumType64 = 'sha256'
  unzipLocation  = $toolsDir
}

Install-ChocolateyZipPackage @packageArgs
# shim is auto-generated for exe in tools; ensure it's on PATH
