#Requires -Version 5.1
<#
.SYNOPSIS
    Automated updater and live-restart script for hyprland-win.
.DESCRIPTION
    1. Pulls the latest commits from origin/main.
    2. Safely stages active .exe files in target/release to .old.exe so compilation succeeds while running.
    3. Rebuilds all workspace crates in --release mode.
    4. Restarts hyprland-win and companion daemons (hyprbar, hyprpaper, hyprlauncher, hyprnotify) cleanly.
#>

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $MyInvocation.MyCommand.Definition
Set-Location $RepoRoot

Write-Host "==> [1/4] Fetching latest updates from GitHub..." -ForegroundColor Cyan
git fetch origin main
git pull --ff-only origin main

Write-Host "==> [2/4] Staging active release binaries..." -ForegroundColor Cyan
Get-ChildItem -Path (Join-Path $RepoRoot "target\release\*.old.exe") -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
foreach ($bin in @("hyprland-win", "hyprwin", "hyprbar", "hyprpaper", "hyprlauncher", "hyprlock", "hyprnotify", "hyprctl", "hyprshot")) {
    $exe = Join-Path $RepoRoot "target\release\$bin.exe"
    $old = Join-Path $RepoRoot "target\release\$bin.old.exe"
    if (Test-Path $exe) {
        Move-Item -Path $exe -Destination $old -Force -ErrorAction SilentlyContinue
    }
}

Write-Host "==> [3/4] Compiling release binaries (cargo build --release --bins)..." -ForegroundColor Cyan
cargo build --release --bins
if ($LASTEXITCODE -ne 0) {
    throw "cargo build --release --bins failed with exit code $LASTEXITCODE"
}

$MainExe = Join-Path $RepoRoot "target\release\hyprland-win.exe"
$CompatExe = Join-Path $RepoRoot "target\release\hyprwin.exe"
Copy-Item -Path $MainExe -Destination $CompatExe -Force

Write-Host "==> [4/4] Restarting hyprland-win & companion daemons..." -ForegroundColor Cyan
Set-Content -Path "$env:TEMP\hyprland_win_boot_unlocked.flag" -Value "1" -NoNewline
schtasks /End /TN "HyprWinBootLock" 2>$null | Out-Null
Stop-Process -Name "hyprland-win","hyprwin","hyprbar","hyprpaper","hyprlauncher","hyprnotify" -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
Get-ChildItem -Path (Join-Path $RepoRoot "target\release\*.old.exe") -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

if (schtasks /Query /TN "HyprWinBootLock" 2>$null) {
    schtasks /Run /TN "HyprWinBootLock" | Out-Null
} else {
    Start-Process -FilePath $MainExe -WindowStyle Hidden
}

Start-Sleep -Seconds 2
& (Join-Path $RepoRoot "target\release\hyprctl.exe") reload
& (Join-Path $RepoRoot "target\release\hyprctl.exe") status
Write-Host "==> hyprland-win updated and restarted!" -ForegroundColor Green
