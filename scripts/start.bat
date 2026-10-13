@echo off
chcp 65001 >nul
setlocal
title MOSS 激光创作系统 · 启动器
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
echo [提示] 未知参数: %~1，默认以桌面模式启动。
shift
goto parse_args

:run_powershell
"%PS_EXE%" -NoProfile -ExecutionPolicy Bypass -Command "$path = $env:MOSS_START_SCRIPT; $lines = Get-Content -LiteralPath $path -Encoding UTF8; $marker = [Array]::IndexOf($lines, '# POWERSHELL-BEGIN'); if ($marker -lt 0) { throw 'Embedded PowerShell section not found.' }; $script = ($lines[($marker + 1)..($lines.Length - 1)] -join [Environment]::NewLine); & ([scriptblock]::Create($script))"
set "EXIT_CODE=%ERRORLEVEL%"

if not "%EXIT_CODE%"=="0" (
    echo.
    echo [错误] MOSS 启动异常退出 (代码: %EXIT_CODE%)
    echo.
    pause
)
exit /b %EXIT_CODE%

# POWERSHELL-BEGIN
$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path (Split-Path -Parent $env:MOSS_START_SCRIPT) "..")).Path
Set-Location -LiteralPath $Root

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "         MOSS 激光创作系统 (Rust 550W 核心)" -ForegroundColor Cyan
Write-Host "==========================================================" -ForegroundColor Cyan

# 查找可执行程序
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

$DesktopExe = Find-Exe "moss-desktop"
$HostExe = Find-Exe "moss-host"

if (-not $DesktopExe -and -not $HostExe) {
    Write-Host "[信息] 未检测到已编译的可执行程序，正在自动执行 Release 构建..." -ForegroundColor Yellow
    cargo build --release -p moss-desktop -p moss-host
    $DesktopExe = Find-Exe "moss-desktop"
    $HostExe = Find-Exe "moss-host"
}

if ($env:MOSS_START_MODE -eq "server") {
    if (-not $HostExe) {
        Write-Host "[错误] 找不到 moss-host.exe，请先运行 cargo build --release -p moss-host" -ForegroundColor Red
        exit 1
    }
    Write-Host "[启动] 正在以独立后台服务模式启动 MOSS Host..." -ForegroundColor Green
    function Run-HostAndOpenBrowser {
        & $HostExe serve 2>&1 | ForEach-Object {
            $line = $_.ToString()
            Write-Host $line
            if ($line -match 'moss-host serving on (?<host>[^:]+):(?<port>[0-9]+)') {
                $url = "http://$($Matches['host']):$($Matches['port'])/"
                Write-Host "[地址] $url" -ForegroundColor Cyan
                Start-Process $url
            }
        }
    }
    Run-HostAndOpenBrowser
} else {
    if ($DesktopExe) {
        Write-Host "[启动] 正在启动 MOSS 原生桌面创作系统..." -ForegroundColor Green
        & $DesktopExe
    } elseif ($HostExe) {
        Write-Host "[提示] 未检测到桌面图形客户端，自动切换为服务模式..." -ForegroundColor Yellow
        function Run-HostAndOpenBrowser {
            & $HostExe serve 2>&1 | ForEach-Object {
                $line = $_.ToString()
                Write-Host $line
                if ($line -match 'moss-host serving on (?<host>[^:]+):(?<port>[0-9]+)') {
                    $url = "http://$($Matches['host']):$($Matches['port'])/"
                    Write-Host "[地址] $url" -ForegroundColor Cyan
                    Start-Process $url
                }
            }
        }
        Run-HostAndOpenBrowser
    } else {
        Write-Host "[错误] 无法找到可执行程序！" -ForegroundColor Red
        exit 1
    }
}
