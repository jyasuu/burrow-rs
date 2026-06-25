#!/usr/bin/env pwsh
param(
    [string]$Version = "latest",
    [string]$Repo = "jyasuu/burrow-rs"
)

$ErrorActionPreference = "Stop"
$binaryName = "burrow.exe"
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$binaryPath = Join-Path $scriptDir $binaryName
$target = "x86_64-pc-windows-msvc"
$archiveName = "burrow-$target.tar.gz"

function Download-Release {
    param([string]$downloadUrl, [string]$tag)
    $archivePath = Join-Path $env:TEMP $archiveName
    Write-Host "Downloading $archiveName ($tag) ..." -ForegroundColor Yellow
    Invoke-WebRequest -Uri $downloadUrl -OutFile $archivePath
    Write-Host "Extracting to $scriptDir ..." -ForegroundColor Yellow
    tar -xzf $archivePath -C $scriptDir
    Remove-Item $archivePath
    Write-Host "Installed $binaryName" -ForegroundColor Green
}

if (-not (Test-Path $binaryPath)) {
    $tag = $Version
    if ($Version -eq "latest") {
        Write-Host "Fetching latest release from $Repo ..." -ForegroundColor Yellow
        try {
            $release = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest" -Headers @{"Accept" = "application/json"}
            $tag = $release.tag_name
        } catch {
            Write-Host "Could not fetch latest release. Trying 'latest' redirect ..." -ForegroundColor DarkYellow
            $tag = "latest"
        }
    } elseif ($tag -notmatch "^v") {
        $tag = "v$tag"
    }

    $dlUrl = if ($tag -eq "latest") {
        "https://github.com/$Repo/releases/latest/download/$archiveName"
    } else {
        "https://github.com/$Repo/releases/download/$tag/$archiveName"
    }
    try {
        Download-Release $dlUrl $tag
    } catch {
        Write-Host "ERROR: Could not download burrow Windows binary." -ForegroundColor Red
        Write-Host ""
        Write-Host "No releases found at https://github.com/$Repo/releases" -ForegroundColor Yellow
        Write-Host ""
        Write-Host "Options:" -ForegroundColor Cyan
        Write-Host "  1. Build from source: cargo build --release --bin burrow" -ForegroundColor White
        Write-Host "  2. Visit the releases page and download manually:" -ForegroundColor White
        Write-Host "     https://github.com/$Repo/releases" -ForegroundColor White
        Write-Host "  3. Place $binaryName manually in this directory" -ForegroundColor White
        pause
        exit 1
    }
}

$port = if ($env:BURROW_PORT) { $env:BURROW_PORT } else { "8080" }
$env:PORT = $port
Write-Host "Starting burrow server on port $port ..." -ForegroundColor Cyan
Write-Host "Tunnel endpoint: ws://localhost:$port/tunnel/ws" -ForegroundColor Cyan
& $binaryPath server
if ($LASTEXITCODE -ne 0) {
    Write-Host "burrow exited with code $LASTEXITCODE" -ForegroundColor Red
    pause
}
