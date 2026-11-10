@echo off
setlocal
title MOSS Laser Control Launcher
cd /d "%~dp0.."

where pwsh >nul 2>nul
if not errorlevel 1 (
    set "PS_EXE=pwsh"
) else (
    set "PS_EXE=powershell"
)

set "MOSS_START_MODE=desktop"
set "MOSS_START_SCRIPT=%~f0"

:parse_args
if "%~1"=="" goto run_powershell
if /I "%~1"=="-Desktop" (
    set "MOSS_START_MODE=desktop"
    shift
    goto parse_args
)
if /I "%~1"=="-Dev" (
    set "MOSS_START_MODE=dev"
    shift
    goto parse_args
)
if /I "%~1"=="-Update" (
    set "MOSS_START_MODE=update"
    shift
    goto parse_args
)
if /I "%~1"=="-Server" (
    set "MOSS_START_MODE=server"
    shift
    goto parse_args
)
if /I "%~1"=="-Host" (
    set "MOSS_START_MODE=server"
    shift
    goto parse_args
)
if /I "%~1"=="-Help" (
    echo Usage: scripts\start.bat [-Desktop ^| -Dev ^| -Update ^| -Server ^| -Host]
    exit /b 0
)
echo [WARN] Unknown option "%~1"; defaulting to desktop mode.
shift
goto parse_args

:run_powershell
"%PS_EXE%" -NoProfile -ExecutionPolicy Bypass -Command "$path = $env:MOSS_START_SCRIPT; $lines = Get-Content -LiteralPath $path -Encoding UTF8; $marker = [Array]::IndexOf($lines, '# POWERSHELL-BEGIN'); if ($marker -lt 0) { throw 'Embedded PowerShell section not found.' }; $script = ($lines[($marker + 1)..($lines.Length - 1)] -join [Environment]::NewLine); & ([scriptblock]::Create($script))"
set "EXIT_CODE=%ERRORLEVEL%"

if not "%EXIT_CODE%"=="0" (
    echo.
    echo [ERROR] MOSS launcher exited with code %EXIT_CODE%.
    echo.
    pause
)
exit /b %EXIT_CODE%

# POWERSHELL-BEGIN
$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path (Split-Path -Parent $env:MOSS_START_SCRIPT) "..")).Path
Set-Location -LiteralPath $Root

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "         MOSS Laser Control System (Rust Core)" -ForegroundColor Cyan
Write-Host "==========================================================" -ForegroundColor Cyan

function Find-Exe {
    param([string]$Name)
    $candidates = @(
        (Join-Path $Root "target\release\$Name.exe"),
        (Join-Path $Root "target\debug\$Name.exe")
    )
    foreach ($cand in $candidates) {
        if (Test-Path -LiteralPath $cand) {
            return $cand
        }
    }
    return $null
}

function Invoke-HostAndOpenBrowser {
    param([string]$ExePath)
    & $ExePath serve 2>&1 | ForEach-Object {
        $line = $_.ToString()
        Write-Host $line
        if ($line -match 'moss-host serving on (?<host>[^:]+):(?<port>[0-9]+)') {
            $url = "http://$($Matches['host']):$($Matches['port'])/"
            Write-Host "[URL] $url" -ForegroundColor Cyan
            Start-Process $url
        }
    }
}

if ($env:MOSS_START_MODE -eq "dev") {
    & (Join-Path $Root "scripts\dev-desktop.ps1")
    exit $LASTEXITCODE
}

if ($env:MOSS_START_MODE -eq "update") {
    & (Join-Path $Root "scripts\quick-update-desktop.ps1") -Launch
    exit $LASTEXITCODE
}

$DesktopExe = Find-Exe "moss-desktop"
$HostExe = Find-Exe "moss-host"

if (-not $DesktopExe -and -not $HostExe) {
    Write-Host "[INFO] No compiled binary found; building release targets..." -ForegroundColor Yellow
    cargo build --release -p moss-desktop -p moss-host
    $DesktopExe = Find-Exe "moss-desktop"
    $HostExe = Find-Exe "moss-host"
}

if ($env:MOSS_START_MODE -eq "server") {
    if (-not $HostExe) {
        Write-Host "[ERROR] moss-host.exe not found; run: cargo build --release -p moss-host" -ForegroundColor Red
        exit 1
    }
    Write-Host "[START] Starting MOSS Host server..." -ForegroundColor Green
    Invoke-HostAndOpenBrowser -ExePath $HostExe
} elseif ($DesktopExe) {
    $ProtoFile = Join-Path $Root "crates\moss-voice-local-proto\src\lib.rs"
    if (Test-Path -LiteralPath $ProtoFile -PathType Leaf) {
        $ProtoRaw = Get-Content -LiteralPath $ProtoFile -Raw
        if ($ProtoRaw -match 'pub\s+const\s+PROTOCOL_VERSION\s*:\s*u32\s*=\s*(\d+)\s*;') {
            $ExpectedProto = [int]$Matches[1]
            $TargetHelper = Join-Path (Split-Path -Parent $DesktopExe) "moss-voice-local.exe"
            function Get-HelperProto([string]$PathToCheck) {
                if (-not (Test-Path -LiteralPath $PathToCheck -PathType Leaf)) { return -1 }
                try {
                    $VerText = (& $PathToCheck --version 2>&1) -join ' '
                    if ($LASTEXITCODE -eq 0 -and $VerText -match '\(protocol\s+(\d+)\b') { return [int]$Matches[1] }
                } catch {}
                return -1
            }
            if ((Get-HelperProto $TargetHelper) -ne $ExpectedProto) {
                $HelperCandidates = @(
                    (Join-Path $Root "target\x86_64-pc-windows-msvc\release\moss-voice-local.exe"),
                    (Join-Path $Root "apps\desktop\src-tauri\binaries\moss-voice-local-x86_64-pc-windows-msvc.exe"),
                    (Join-Path $Root "target\release\moss-voice-local.exe")
                )
                $MatchedHelper = $null
                foreach ($Cand in $HelperCandidates) {
                    if ((Get-HelperProto $Cand) -eq $ExpectedProto) { $MatchedHelper = $Cand; break }
                }
                if ($null -eq $MatchedHelper) {
                    Write-Host "[INFO] Building moss-voice-local helper (protocol $ExpectedProto)..." -ForegroundColor Yellow
                    cargo build --release --manifest-path (Join-Path $Root "apps\moss-voice-local\Cargo.toml") --target x86_64-pc-windows-msvc --target-dir (Join-Path $Root "target")
                    $BuiltHelper = Join-Path $Root "target\x86_64-pc-windows-msvc\release\moss-voice-local.exe"
                    if ((Get-HelperProto $BuiltHelper) -eq $ExpectedProto) { $MatchedHelper = $BuiltHelper }
                }
                if ($null -ne $MatchedHelper -and $MatchedHelper -ine $TargetHelper) {
                    if (Test-Path -LiteralPath $TargetHelper -PathType Leaf) {
                        try {
                            $LockStream = [System.IO.File]::Open($TargetHelper, 'Open', 'ReadWrite', 'None')
                            $LockStream.Close()
                        } catch {
                            $Stamp = (Get-Date).ToString('yyyyMMddHHmmss')
                            Move-Item -LiteralPath $TargetHelper -Destination (Join-Path (Split-Path -Parent $TargetHelper) "moss-voice-local.old-$Stamp.exe") -Force
                        }
                    }
                    Copy-Item -LiteralPath $MatchedHelper -Destination $TargetHelper -Force
                    Write-Host "[SYNC] Updated moss-voice-local.exe (protocol $ExpectedProto)" -ForegroundColor Green
                }
            }
        }
    }
    Write-Host "[START] Launching MOSS Desktop..." -ForegroundColor Green
    & $DesktopExe
} elseif ($HostExe) {
    Write-Host "[INFO] Desktop binary not found; falling back to MOSS Host server..." -ForegroundColor Yellow
    Invoke-HostAndOpenBrowser -ExePath $HostExe
} else {
    Write-Host "[ERROR] Executable not found." -ForegroundColor Red
    exit 1
}
