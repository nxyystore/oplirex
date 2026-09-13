# oplire unified Windows installer — oplirex + optional WARP
param(
    [switch]$Force,
    [switch]$Verbose,
    [switch]$NoWarp,
    [string]$Version = "latest"
)

$ErrorActionPreference = "Stop"
if ($Verbose) { Write-Host "[VERBOSE] Installation started" -ForegroundColor Cyan }

function Test-Admin {
    return ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# --- install oplirex ---
function Install-Oplirex {
    $binDir = "$env:ProgramFiles\oplirex"
    $exe = Join-Path $binDir "oplirex.exe"
    if ((Test-Path $exe) -and -not $Force) {
        Write-Host "oplirex already installed at $exe" -ForegroundColor Green
        try { & $exe --version } catch {}
        return
    }

    # Prefer winget if available and not forced to direct download
    if ((Get-Command winget -ErrorAction SilentlyContinue) -and -not $Force) {
        Write-Host "Trying winget install nxyy.oplirex..." -ForegroundColor Cyan
        try { winget install --id nxyy.oplirex --silent --accept-package-agreements --accept-source-agreements; if (Test-Path $exe) { return } } catch { Write-Host "winget failed, falling back to direct download" -ForegroundColor Yellow }
    }

    $tag = if ($Version -eq "latest") { "latest/download" } else { "download/$Version" }
    # Prefer MSI if available, fallback to exe zip
    $msiUrl = "https://github.com/nxyystore/oplire/releases/$tag/oplirex-windows-x86_64.msi"
    $zipUrl = "https://github.com/nxyystore/oplire/releases/$tag/oplirex-windows-x86_64.zip"
    $exeUrl = "https://github.com/nxyystore/oplire/releases/$tag/oplirex-windows-x86_64.exe"

    $tmp = Join-Path $env:TEMP "oplirex-install"
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null

    # Try MSI first
    $msiPath = Join-Path $tmp "oplirex.msi"
    try {
        Write-Host "Downloading $msiUrl ..." -ForegroundColor Cyan
        Invoke-WebRequest -Uri $msiUrl -OutFile $msiPath -UseBasicParsing -ErrorAction Stop
        if ((Get-Item $msiPath).Length -gt 10000) {
            Write-Host "Installing MSI..." -ForegroundColor Cyan
            if (-not (Test-Admin)) { throw "MSI install requires Administrator" }
            Start-Process msiexec.exe -ArgumentList "/i `"$msiPath`" /qn /norestart" -Wait
            Remove-Item $msiPath -Force -ErrorAction SilentlyContinue
            Write-Host "oplirex installed via MSI" -ForegroundColor Green
            return
        }
    } catch { Write-Host "MSI not available or failed: $_" -ForegroundColor Yellow }

    # Fallback: zip or exe
    $zipPath = Join-Path $tmp "oplirex.zip"
    try {
        Write-Host "Downloading $zipUrl ..." -ForegroundColor Cyan
        Invoke-WebRequest -Uri $zipUrl -OutFile $zipPath -UseBasicParsing -ErrorAction Stop
        Expand-Archive -Path $zipPath -DestinationPath $tmp -Force
        $found = Get-ChildItem -Path $tmp -Filter "oplirex.exe" -Recurse | Select-Object -First 1
        if ($null -eq $found) { throw "oplirex.exe not found in zip" }
        New-Item -ItemType Directory -Force -Path $binDir | Out-Null
        Copy-Item $found.FullName $exe -Force
    } catch {
        Write-Host "Zip failed, trying direct exe $exeUrl ..." -ForegroundColor Yellow
        try {
            Invoke-WebRequest -Uri $exeUrl -OutFile $exe -UseBasicParsing -ErrorAction Stop
        } catch { Write-Host "Failed to download oplirex: $_" -ForegroundColor Red; exit 1 }
        New-Item -ItemType Directory -Force -Path $binDir | Out-Null
        if (-not (Test-Path $exe)) {
            $tmpExe = Join-Path $tmp "oplirex.exe"
            Invoke-WebRequest -Uri $exeUrl -OutFile $tmpExe -UseBasicParsing
            Copy-Item $tmpExe $exe -Force
        }
    }

    # Add to PATH
    $currentPath = [Environment]::GetEnvironmentVariable("Path", "Machine")
    if ($currentPath -notlike "*$binDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$currentPath;$binDir", "Machine")
        $env:Path += ";$binDir"
        Write-Host "Added $binDir to system PATH" -ForegroundColor Green
    }
    Write-Host "oplirex installed at $exe" -ForegroundColor Green
    try { & $exe --version } catch {}
}

# --- install WARP (optional) ---
function Install-Warp {
    if ($NoWarp) { Write-Host "Skipping WARP (--NoWarp)" -ForegroundColor Yellow; return }
    $warp1 = "${env:ProgramFiles(x86)}\Cloudflare\Cloudflare WARP\Cloudflare WARP.exe"
    $warp2 = "${env:ProgramFiles}\Cloudflare\Cloudflare WARP\Cloudflare WARP.exe"
    if (((Test-Path $warp1) -or (Test-Path $warp2)) -and -not $Force) {
        Write-Host "Cloudflare WARP already installed" -ForegroundColor Green
        return
    }
    if (-not (Test-Admin)) { Write-Host "WARP install requires Administrator — skipping (run as admin or use --NoWarp)" -ForegroundColor Yellow; return }
    Write-Host "Installing Cloudflare WARP..." -ForegroundColor Green
    $url = "https://1111-releases.cloudflare.com/latest/Windows%20Cloudflare%20WARP%20Setup.msi"
    $altUrl = "https://1111-releases.cloudflare.com/latest/Windows%20Cloudflare%20WARP%20Setup.exe"
    $tmp = Join-Path $env:TEMP "warp-install.msi"
    try {
        Write-Host "Downloading WARP..." -ForegroundColor Cyan
        try { Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing -ErrorAction Stop }
        catch { Invoke-WebRequest -Uri $altUrl -OutFile $tmp -UseBasicParsing -ErrorAction Stop }
        if ($tmp -like "*.msi") {
            Start-Process msiexec.exe -ArgumentList "/i `"$tmp`" /qn /norestart" -Wait
        } else {
            # exe uses /S for silent
            Start-Process -FilePath $tmp -ArgumentList "/S" -Wait
        }
        Remove-Item $tmp -Force -ErrorAction SilentlyContinue
        Write-Host "Cloudflare WARP installed" -ForegroundColor Green
    } catch { Write-Host "WARP install failed: $_" -ForegroundColor Red }
}

Install-Oplirex
Install-Warp
Write-Host ""
Write-Host "Done. Try: oplirex --version  /  oplire status" -ForegroundColor Cyan
