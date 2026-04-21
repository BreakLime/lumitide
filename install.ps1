#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Windows installer for lumitide. Pure PowerShell, no Inno Setup required.
.DESCRIPTION
    Downloads the portable lumitide-windows.exe release asset, installs it
    user-scoped under $env:LOCALAPPDATA\Programs\lumitide, updates the User
    PATH, and (optionally) creates a Start Menu shortcut.

    First install:  irm https://raw.githubusercontent.com/BreakLime/lumitide/main/install.ps1 | iex
    Re-install:     .\install.ps1
    Pin version:    .\install.ps1 -Version v1.0.8
    Skip shortcut:  .\install.ps1 -NoShortcut
    Verbose:        .\install.ps1 -VerboseLog
#>
param(
    [string]$Version,
    [string]$Prefix,
    [switch]$NoShortcut,
    [switch]$VerboseLog
)

$ErrorActionPreference = 'Stop'

$Repo = 'BreakLime/lumitide'
$Asset = 'lumitide-windows.exe'

function Abort([string]$msg) {
    Write-Host "error: $msg" -ForegroundColor Red
    exit 1
}

# --- Arch guard ---
if (-not [Environment]::Is64BitOperatingSystem) {
    Abort "no prebuilt binary for 32-bit Windows — build from source: https://github.com/$Repo#build-from-source"
}

# --- Refuse if an Inno wizard install already exists ---
# Inno's lumitide.iss uses DefaultDirName={autopf}\Lumitide, so per-machine
# installs land under Program Files (x86/native) and per-user installs land
# under $env:LOCALAPPDATA\Programs\Lumitide. Check all three.
$innoCandidates = @(
    (Join-Path $env:ProgramFiles        'Lumitide\lumitide.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Lumitide\lumitide.exe'),
    (Join-Path $env:LOCALAPPDATA        'Programs\Lumitide\lumitide.exe')
) | Where-Object { $_ -and (Test-Path $_) }

if ($innoCandidates) {
    $pathList = ($innoCandidates | ForEach-Object { "  $_" }) -join "`n"
    Abort @"
detected an existing Lumitide wizard install at:
$pathList
uninstall it first from Settings > Apps > Installed apps, then re-run this script.
(to keep using the wizard installer instead, skip this script and download
 lumitide-installer.exe from https://github.com/$Repo/releases)
"@
}

# --- Resolve install dir ---
$InstallDir = if ($Prefix) {
    $Prefix
} elseif ($env:LUMITIDE_PREFIX) {
    $env:LUMITIDE_PREFIX
} else {
    Join-Path $env:LOCALAPPDATA 'Programs\lumitide'
}
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

# --- Download URL ---
$url = if ($Version) {
    "https://github.com/$Repo/releases/download/$Version/$Asset"
} else {
    "https://github.com/$Repo/releases/latest/download/$Asset"
}

# --- Download ---
$tmp = Join-Path $InstallDir ("lumitide-" + [guid]::NewGuid() + ".exe")
try {
    $prev = $ProgressPreference
    $ProgressPreference = if ($VerboseLog) { 'Continue' } else { 'SilentlyContinue' }
    if ($VerboseLog) {
        Write-Host "Downloading $url"
    } else {
        Write-Host -NoNewline "Downloading lumitide... "
    }
    try {
        Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
    } catch {
        if (-not $VerboseLog) { Write-Host "FAILED" -ForegroundColor Red }
        Abort "download failed: $url`n$($_.Exception.Message)"
    } finally {
        $ProgressPreference = $prev
    }
    if (-not $VerboseLog) { Write-Host "done" }

    if (-not (Test-Path $tmp) -or (Get-Item $tmp).Length -eq 0) {
        Abort "downloaded file is empty: $url"
    }

    # --- Checksum verification ---
    $checksumsUrl = $url -replace '[^/]+$', 'sha256sums.txt'
    $tmpSums = Join-Path $env:TEMP ("lumitide-sums-" + [guid]::NewGuid() + ".txt")
    try {
        Invoke-WebRequest -Uri $checksumsUrl -OutFile $tmpSums -UseBasicParsing -ErrorAction Stop
        $expected = (Get-Content $tmpSums | Where-Object { $_ -match "\s$([regex]::Escape($Asset))$" }) -replace '^(\S+)\s.*', '$1'
        if ($expected) {
            $actual = (Get-FileHash $tmp -Algorithm SHA256).Hash.ToLower()
            if ($actual -ne $expected.ToLower()) {
                Abort "checksum mismatch for $Asset`n  expected: $expected`n  got:      $actual"
            }
            if ($VerboseLog) { Write-Host "Checksum OK: $actual" }
        } else {
            Write-Host "warning: $Asset not found in sha256sums.txt — skipping verification" -ForegroundColor Yellow
        }
    } catch {
        Write-Host "warning: could not fetch sha256sums.txt — skipping checksum verification" -ForegroundColor Yellow
    } finally {
        if (Test-Path $tmpSums) { Remove-Item -Force $tmpSums -ErrorAction SilentlyContinue }
    }

    # --- Install (atomic-ish) ---
    $dest = Join-Path $InstallDir 'lumitide.exe'
    try {
        Move-Item -Force -Path $tmp -Destination $dest
    } catch {
        Abort "could not replace $dest — is lumitide running? close it and re-run.`n$($_.Exception.Message)"
    }
} finally {
    if (Test-Path $tmp) { Remove-Item -Force $tmp -ErrorAction SilentlyContinue }
}

# --- PATH update (user scope, no admin) ---
$pathAdded = $false
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$userPathEntries = if ($userPath) { $userPath -split ';' } else { @() }
if ($userPathEntries -inotcontains $InstallDir) {
    $new = if ([string]::IsNullOrEmpty($userPath)) {
        $InstallDir
    } else {
        "$userPath;$InstallDir"
    }
    [Environment]::SetEnvironmentVariable('Path', $new, 'User')
    $env:Path = "$env:Path;$InstallDir"
    $pathAdded = $true
}

# --- Start Menu shortcut (best effort) ---
if (-not $NoShortcut) {
    try {
        $startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
        if (-not (Test-Path $startMenu)) {
            New-Item -ItemType Directory -Force -Path $startMenu | Out-Null
        }
        $lnk = Join-Path $startMenu 'Lumitide.lnk'
        $shell = New-Object -ComObject WScript.Shell
        $s = $shell.CreateShortcut($lnk)
        $s.TargetPath = $dest
        $s.WorkingDirectory = $InstallDir
        $s.IconLocation = $dest
        $s.Save()
    } catch {
        Write-Host "warning: could not create Start Menu shortcut: $($_.Exception.Message)" -ForegroundColor Yellow
    }
}

# --- Self-check: `lumitide --help` exits 0 iff the binary loads cleanly ---
Write-Host "Installed lumitide -> $dest"
try {
    & $dest --help *> $null
    if ($LASTEXITCODE -ne 0) {
        Write-Host "warning: '$dest --help' exited nonzero" -ForegroundColor Yellow
    }
} catch {
    Write-Host "warning: self-check failed: $($_.Exception.Message)" -ForegroundColor Yellow
}

if ($pathAdded) {
    Write-Host ""
    Write-Host "Added $InstallDir to your User PATH. Open a new terminal to pick it up."
}

Write-Host ""
Write-Host "Run 'lumitide' to start."
