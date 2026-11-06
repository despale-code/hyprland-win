#Requires -Version 5.1
<#
.SYNOPSIS
    Automated installer and system integration script for hyprland-win.
.DESCRIPTION
    1. Installs missing dependencies (Rust toolchain, Alacritty, Oh My Posh, Fastfetch) via winget.
    2. Installs bundled SF Pro & JetBrainsMono Nerd Fonts into user fonts.
    3. Seeds ~/.config/hyprwin configuration files without overwriting existing customizations.
    4. Configures Alacritty (borderless, 32px padding, SFMono font, user home working directory)
       and PowerShell profile (Oh My Posh agnosterplus theme + fastfetch on startup).
    5. Compiles all hyprland-win workspace binaries in --release mode.
    6. Registers Windows Logon Scheduled Task (HyprWinBootLock) and launches hyprland-win.
#>

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $MyInvocation.MyCommand.Definition
Set-Location $RepoRoot

Write-Host "==> [1/6] Checking prerequisites..." -ForegroundColor Cyan

function Ensure-WingetPackage {
    param([string]$CommandName, [string]$PackageId, [string]$DisplayName)
    if (-not (Get-Command $CommandName -ErrorAction SilentlyContinue)) {
        Write-Host "    Installing $DisplayName ($PackageId) via winget..." -ForegroundColor Yellow
        winget install --id $PackageId --exact --silent --accept-package-agreements --accept-source-agreements
    } else {
        Write-Host "    [OK] $DisplayName is installed." -ForegroundColor Green
    }
}

Ensure-WingetPackage -CommandName "cargo" -PackageId "Rustlang.Rustup" -DisplayName "Rust Toolchain"
Ensure-WingetPackage -CommandName "alacritty" -PackageId "Alacritty.Alacritty" -DisplayName "Alacritty Terminal"
Ensure-WingetPackage -CommandName "oh-my-posh" -PackageId "JanDeDobbeleer.OhMyPosh" -DisplayName "Oh My Posh"
Ensure-WingetPackage -CommandName "fastfetch" -PackageId "Fastfetch-cli.Fastfetch" -DisplayName "Fastfetch"

Write-Host "==> [2/6] Installing bundled fonts..." -ForegroundColor Cyan
$UserFontDir = Join-Path $env:LOCALAPPDATA "Microsoft\Windows\Fonts"
if (-not (Test-Path $UserFontDir)) {
    New-Item -ItemType Directory -Path $UserFontDir -Force | Out-Null
}
$FontRegKey = "HKCU:\Software\Microsoft\Windows NT\CurrentVersion\Fonts"
$AssetsFontDir = Join-Path $RepoRoot "assets\fonts"
if (Test-Path $AssetsFontDir) {
    Get-ChildItem -Path $AssetsFontDir -File -Include *.ttf,*.otf | ForEach-Object {
        $dst = Join-Path $UserFontDir $_.Name
        if (-not (Test-Path $dst)) {
            Copy-Item -Path $_.FullName -Destination $dst -Force
            New-ItemProperty -Path $FontRegKey -Name $_.BaseName -Value $dst -PropertyType String -Force | Out-Null
        }
    }
}
Write-Host "    [OK] Bundled fonts verified." -ForegroundColor Green

Write-Host "==> [3/6] Seeding configuration in ~/.config/hyprwin..." -ForegroundColor Cyan
$ConfigDir = Join-Path $env:USERPROFILE ".config\hyprwin"
if (-not (Test-Path $ConfigDir)) {
    New-Item -ItemType Directory -Path $ConfigDir -Force | Out-Null
}
$RepoConfigDir = Join-Path $RepoRoot "config"
if (Test-Path $RepoConfigDir) {
    Get-ChildItem -Path $RepoConfigDir -File | ForEach-Object {
        $targetFile = Join-Path $ConfigDir $_.Name
        if (-not (Test-Path $targetFile)) {
            Copy-Item -Path $_.FullName -Destination $targetFile -Force
            Write-Host "    Seeded: $targetFile" -ForegroundColor DarkGray
        }
    }
}
Write-Host "    [OK] Configuration directory ready at $ConfigDir" -ForegroundColor Green

Write-Host "==> [4/6] Configuring Alacritty & PowerShell profile..." -ForegroundColor Cyan
$AlacrittyDir = Join-Path $env:APPDATA "alacritty"
if (-not (Test-Path $AlacrittyDir)) {
    New-Item -ItemType Directory -Path $AlacrittyDir -Force | Out-Null
}
$AlacrittyToml = Join-Path $AlacrittyDir "alacritty.toml"
$EscapedHome = $env:USERPROFILE.Replace("\", "\\")
if (-not (Test-Path $AlacrittyToml)) {
    @"
[general]
working_directory = "$EscapedHome"

[window]
decorations = "None"
padding = { x = 32, y = 32 }
dynamic_padding = false
opacity = 0.96

[font]
size = 11.0

[font.normal]
family = "DepartureMono Nerd Font"
style = "Regular"

[font.bold]
family = "DepartureMono Nerd Font"
style = "Regular"

[font.italic]
family = "DepartureMono Nerd Font"
style = "Regular"

[font.bold_italic]
family = "DepartureMono Nerd Font"
style = "Regular"

[terminal.shell]
program = "powershell.exe"
args = ["-NoLogo"]

[colors.primary]
background = "#121318"
foreground = "#F4F4F6"
"@ | Set-Content -Path $AlacrittyToml -Encoding UTF8
}

$OmpThemeDir = Join-Path $env:POSH_THEMES_PATH ""
$AgnosterUrl = "https://raw.githubusercontent.com/JanDeDobbeleer/oh-my-posh/main/themes/agnosterplus.omp.json"
$CustomOmpTheme = Join-Path $ConfigDir "agnosterplus.omp.json"
if (-not (Test-Path $CustomOmpTheme)) {
    try {
        Invoke-WebRequest -Uri $AgnosterUrl -UseBasicParsing -OutFile $CustomOmpTheme
    } catch {}
}

$ProfilePath = $PROFILE.CurrentUserAllHosts
$ProfileParent = Split-Path -Parent $ProfilePath
if (-not (Test-Path $ProfileParent)) {
    New-Item -ItemType Directory -Path $ProfileParent -Force | Out-Null
}
if (-not (Test-Path $ProfilePath)) {
    New-Item -ItemType File -Path $ProfilePath -Force | Out-Null
}
$ProfileContent = Get-Content -Path $ProfilePath -Raw -ErrorAction SilentlyContinue
if ($null -eq $ProfileContent -or $ProfileContent -notmatch "oh-my-posh") {
    Add-Content -Path $ProfilePath -Value "`nif (Get-Command oh-my-posh -ErrorAction SilentlyContinue) { oh-my-posh init pwsh --config `"$CustomOmpTheme`" | Invoke-Expression }`nif (Get-Command fastfetch -ErrorAction SilentlyContinue) { fastfetch }`n"
}
Write-Host "    [OK] Alacritty & PowerShell profile configured." -ForegroundColor Green

Write-Host "==> [5/6] Building hyprland-win workspace binaries (--release)..." -ForegroundColor Cyan
Get-ChildItem -Path (Join-Path $RepoRoot "target\release\*.old.exe") -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
foreach ($bin in @("hyprland-win", "hyprwin", "hyprbar", "hyprpaper", "hyprlauncher", "hyprlock", "hyprnotify", "hyprctl", "hyprshot")) {
    $exe = Join-Path $RepoRoot "target\release\$bin.exe"
    $old = Join-Path $RepoRoot "target\release\$bin.old.exe"
    if (Test-Path $exe) {
        Move-Item -Path $exe -Destination $old -Force -ErrorAction SilentlyContinue
    }
}

cargo build --release --bins
if ($LASTEXITCODE -ne 0) {
    throw "cargo build --release --bins failed with exit code $LASTEXITCODE"
}

$MainExe = Join-Path $RepoRoot "target\release\hyprland-win.exe"
$CompatExe = Join-Path $RepoRoot "target\release\hyprwin.exe"
Copy-Item -Path $MainExe -Destination $CompatExe -Force
Write-Host "    [OK] Release binaries compiled." -ForegroundColor Green

Write-Host "==> [6/6] Registering logon integration & starting hyprland-win..." -ForegroundColor Cyan
$IsAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ($IsAdmin) {
    schtasks /Create /TN "HyprWinBootLock" /TR "`"$MainExe`" --boot-lock" /SC ONLOGON /RL HIGHEST /F | Out-Null
    Write-Host "    [OK] Registered Scheduled Task 'HyprWinBootLock' (ONLOGON, Highest Privileges)." -ForegroundColor Green
} else {
    schtasks /Create /TN "HyprWinBootLock" /TR "`"$MainExe`" --boot-lock" /SC ONLOGON /F 2>$null | Out-Null
    Write-Host "    [INFO] Registered Logon Task (run setup.ps1 as Administrator for /RL HIGHEST elevation)." -ForegroundColor Yellow
}

Set-Content -Path "$env:TEMP\hyprland_win_boot_unlocked.flag" -Value "1" -NoNewline
Stop-Process -Name "hyprland-win","hyprwin","hyprbar","hyprpaper","hyprlauncher","hyprnotify" -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
Get-ChildItem -Path (Join-Path $RepoRoot "target\release\*.old.exe") -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

if (schtasks /Query /TN "HyprWinBootLock" 2>$null) {
    schtasks /Run /TN "HyprWinBootLock" | Out-Null
} else {
    Start-Process -FilePath $MainExe -WindowStyle Hidden
}

Start-Sleep -Seconds 2
& (Join-Path $RepoRoot "target\release\hyprctl.exe") status
Write-Host "==> hyprland-win setup complete!" -ForegroundColor Green
