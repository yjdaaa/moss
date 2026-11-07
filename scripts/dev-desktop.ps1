# Launch MOSS desktop in debug hot-reload mode (Vite 127.0.0.1:5177 + native Tauri window).
# Usage: powershell -File scripts/dev-desktop.ps1 [-RebuildDist] [-DryRun]
# Pure ASCII script for Windows PowerShell 5.1 compatibility.
[CmdletBinding()]
param(
    [switch]$RebuildDist,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$frontendDir = Join-Path $repo 'frontend'
$distIndex = Join-Path $frontendDir 'dist/index.html'
$protoSource = Join-Path $repo 'crates/moss-voice-local-proto/src/lib.rs'
$helperManifest = Join-Path $repo 'apps/moss-voice-local/Cargo.toml'
$triple = 'x86_64-pc-windows-msvc'
$debugHelperExe = Join-Path $repo 'target/debug/moss-voice-local.exe'
$releaseHelperExe = Join-Path $repo 'target/release/moss-voice-local.exe'
$tripleHelperExe = Join-Path $repo "target/$triple/release/moss-voice-local.exe"
$sidecarHelperExe = Join-Path $repo "apps/desktop/src-tauri/binaries/moss-voice-local-$triple.exe"
$devHost = '127.0.0.1'
$devPort = 5177
$devOrigin = "http://${devHost}:${devPort}"
$npmCmd = if ($env:OS -eq 'Windows_NT') { 'npm.cmd' } else { 'npm' }

function Get-ExpectedVoiceHelperProtocol {
    $raw = Get-Content -LiteralPath $protoSource -Raw
    if ($raw -match 'pub\s+const\s+PROTOCOL_VERSION\s*:\s*u32\s*=\s*(\d+)\s*;') {
        return [int]$Matches[1]
    }
    throw "Unable to parse PROTOCOL_VERSION from $protoSource"
}

function Get-BinaryVoiceHelperProtocol([string]$exePath) {
    if (-not (Test-Path -LiteralPath $exePath -PathType Leaf)) { return -1 }
    try {
        $out = (& $exePath --version 2>&1) -join ' '
        if ($LASTEXITCODE -ne 0) { return -1 }
        if ($out -match '\(protocol\s+(\d+)\b') {
            return [int]$Matches[1]
        }
        return -1
    } catch {
        return -1
    }
}

function Test-FileLocked([string]$path) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $false }
    try {
        $stream = [System.IO.File]::Open($path, 'Open', 'ReadWrite', 'None')
        $stream.Close()
        return $false
    } catch {
        return $true
    }
}

function Ensure-DevVoiceHelper {
    if ($env:OS -ne 'Windows_NT') { return }
    $expectedProto = Get-ExpectedVoiceHelperProtocol
    if ((Get-BinaryVoiceHelperProtocol $debugHelperExe) -eq $expectedProto) {
        return
    }
    $srcHelper = $null
    foreach ($cand in @($tripleHelperExe, $releaseHelperExe, $sidecarHelperExe)) {
        if ((Get-BinaryVoiceHelperProtocol $cand) -eq $expectedProto) {
            $srcHelper = $cand
            break
        }
    }
    if ($null -eq $srcHelper) {
        Write-Host "[MOSS Dev] Building release moss-voice-local.exe (protocol $expectedProto)..." -ForegroundColor Cyan
        & cargo build --release --manifest-path $helperManifest --target $triple --target-dir (Join-Path $repo 'target')
        if ($LASTEXITCODE -ne 0) { throw 'moss-voice-local build failed.' }
        $srcHelper = $tripleHelperExe
    }
    $debugDir = Split-Path -Parent $debugHelperExe
    New-Item -ItemType Directory -Path $debugDir -Force | Out-Null
    if (Test-FileLocked $debugHelperExe) {
        $stamp = (Get-Date).ToString('yyyyMMddHHmmss')
        Move-Item -LiteralPath $debugHelperExe -Destination (Join-Path $debugDir "moss-voice-local.old-${stamp}.exe") -Force
    }
    Copy-Item -LiteralPath $srcHelper -Destination $debugHelperExe -Force
    Write-Host "[MOSS Dev] Synced voice helper (protocol $expectedProto) -> $debugHelperExe" -ForegroundColor Green
}

function Test-LoopbackPort([string]$hostname, [int]$port) {
    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $async = $client.BeginConnect($hostname, $port, $null, $null)
        if (-not $async.AsyncWaitHandle.WaitOne(250, $false)) {
            return $false
        }
        $client.EndConnect($async)
        return $true
    } catch {
        return $false
    } finally {
        $client.Close()
    }
}

function Invoke-LoopbackGet([string]$url) {
    $resp = $null
    $reader = $null
    try {
        $req = [System.Net.HttpWebRequest]::Create($url)
        $req.Method = 'GET'
        $req.Timeout = 1000
        $req.ReadWriteTimeout = 1000
        $req.AllowAutoRedirect = $false
        $resp = [System.Net.HttpWebResponse]$req.GetResponse()
        if ([int]$resp.StatusCode -ne 200) { return $null }
        $stream = $resp.GetResponseStream()
        if ($null -eq $stream) { return '' }
        $reader = New-Object System.IO.StreamReader($stream, [System.Text.Encoding]::UTF8)
        return $reader.ReadToEnd()
    } catch {
        return $null
    } finally {
        if ($null -ne $reader) { $reader.Dispose() }
        if ($null -ne $resp) { $resp.Close() }
    }
}

function Test-MossViteHttp([string]$origin) {
    $html = Invoke-LoopbackGet "$origin/"
    if ($null -eq $html) { return $false }
    if ($html -notmatch '<title>\s*MOSS\b') { return $false }
    if ($html -notmatch '/src/main\.jsx') { return $false }
    if ($html -notmatch 'id="root"') { return $false }
    $viteClient = Invoke-LoopbackGet "$origin/@vite/client"
    if ($null -eq $viteClient) { return $false }
    return $true
}

function Test-CommandLineMatchesFrontendDir([string]$commandLine, [string]$expectedFrontendDir) {
    if ([string]::IsNullOrWhiteSpace($commandLine) -or [string]::IsNullOrWhiteSpace($expectedFrontendDir)) {
        return $false
    }
    $fullExpected = ([System.IO.Path]::GetFullPath($expectedFrontendDir)).Replace('\', '/').TrimEnd('/')
    $normCmd = $commandLine.Replace('\', '/')
    $pattern = '(?i)(?:^|[\s"'';=])' + [regex]::Escape($fullExpected) + '(?=$|/|[\s"'';])'
    return ($normCmd -match $pattern)
}

function Test-MossViteProcess([int]$port, [string]$expectedFrontendDir) {
    if ($env:OS -ne 'Windows_NT') {
        try {
            $lsofCmd = Get-Command lsof -ErrorAction SilentlyContinue
            $psCmd = Get-Command ps -ErrorAction SilentlyContinue
            if ($null -eq $lsofCmd -or $null -eq $psCmd) { return $false }
            $rawPids = @(& $lsofCmd.Source -nP "-iTCP:$port" -sTCP:LISTEN -t 2>$null)
            if ($rawPids.Count -eq 0) { return $false }
            foreach ($rawPid in $rawPids) {
                $curPid = 0
                if (-not [int]::TryParse([string]$rawPid, [ref]$curPid) -or $curPid -le 0) { return $false }
                $matched = $false
                for ($depth = 0; $depth -lt 4 -and $curPid -gt 0; $depth++) {
                    $line = (& $psCmd.Source -o ppid=,command= -p $curPid 2>$null | Select-Object -First 1)
                    if ([string]::IsNullOrWhiteSpace($line)) { break }
                    $trimmed = ([string]$line).Trim()
                    if ($trimmed -notmatch '^(\d+)\s+(.*)$') { break }
                    $ppid = [int]$Matches[1]
                    $cmd = [string]$Matches[2]
                    if (Test-CommandLineMatchesFrontendDir $cmd $expectedFrontendDir) {
                        $matched = $true
                        break
                    }
                    $curPid = $ppid
                }
                if (-not $matched) { return $false }
            }
            return $true
        } catch {
            return $false
        }
    }
    try {
        $conns = @(Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue)
        if ($conns.Count -eq 0) { return $false }
        $ownerPids = @($conns | Select-Object -ExpandProperty OwningProcess -Unique)
        foreach ($ownerPid in $ownerPids) {
            if ($ownerPid -le 0) { return $false }
            $matched = $false
            $curPid = [int]$ownerPid
            for ($depth = 0; $depth -lt 4 -and $curPid -gt 0; $depth++) {
                $proc = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $curPid" -ErrorAction SilentlyContinue
                if ($null -eq $proc) { break }
                $cmd = [string]$proc.CommandLine
                if (Test-CommandLineMatchesFrontendDir $cmd $expectedFrontendDir) {
                    $matched = $true
                    break
                }
                $curPid = [int]$proc.ParentProcessId
            }
            if (-not $matched) { return $false }
        }
        return $true
    } catch {
        return $false
    }
}

function Test-VerifiedMossViteOrigin([string]$origin, [int]$port, [string]$expectedFrontendDir) {
    return (Test-MossViteProcess $port $expectedFrontendDir) -and (Test-MossViteHttp $origin)
}

$needDist = $RebuildDist -or (-not (Test-Path -LiteralPath $distIndex -PathType Leaf))

$tauriConfPath = Join-Path $repo 'apps/desktop/src-tauri/tauri.conf.json'
$tauriConfObj = Get-Content -LiteralPath $tauriConfPath -Raw | ConvertFrom-Json
$confVersion = [string]$tauriConfObj.version

function Test-NewerThreePartVersion([string]$cand, [string]$base) {
    $mc = [regex]::Match($cand, '^(\d+)\.(\d+)\.(\d+)$')
    $mb = [regex]::Match($base, '^(\d+)\.(\d+)\.(\d+)$')
    if (-not $mc.Success) { return $false }
    if (-not $mb.Success) { return $true }
    for ($i = 1; $i -le 3; $i++) {
        $vc = [int]$mc.Groups[$i].Value
        $vb = [int]$mb.Groups[$i].Value
        if ($vc -gt $vb) { return $true }
        if ($vc -lt $vb) { return $false }
    }
    return $false
}

function Resolve-EffectiveMossVersion([string]$defaultVer) {
    $best = $defaultVer
    try {
        $tags = @(& git -C $repo tag --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-v:refname 2>$null)
        if ($LASTEXITCODE -eq 0) {
            foreach ($t in $tags) {
                $mt = [regex]::Match([string]$t, '^v(\d+\.\d+\.\d+)$')
                if ($mt.Success) {
                    $tagVer = $mt.Groups[1].Value
                    if (Test-NewerThreePartVersion $tagVer $best) {
                        $best = $tagVer
                    }
                    break
                }
            }
        }
    } catch {
        # Ignore missing git binary and fall back to tauri.conf.json version.
    }
    return $best
}

$effectiveVersion = Resolve-EffectiveMossVersion $confVersion

if ($DryRun) {
    Write-Host "[DryRun] Repo: $repo"
    Write-Host "[DryRun] Effective version: $effectiveVersion"
    Write-Host "[DryRun] Need initial frontend/dist build (for packaged index.html): $needDist"
    Write-Host "[DryRun] Dev UI origin: $devOrigin"
    Write-Host "[DryRun] Command: MOSS_DEV_UI_ORIGIN=$devOrigin MOSS_RELEASE_VERSION=$effectiveVersion cargo run -p moss-desktop"
    exit 0
}

Ensure-DevVoiceHelper

$viteProc = $null
$prevDevOrigin = $env:MOSS_DEV_UI_ORIGIN
$prevReleaseVer = $env:MOSS_RELEASE_VERSION
$prevTauriCfg = $env:TAURI_CONFIG
try {
    $env:MOSS_RELEASE_VERSION = $effectiveVersion
    if ($effectiveVersion -ne $confVersion) {
        $env:TAURI_CONFIG = (@{ version = $effectiveVersion } | ConvertTo-Json -Compress)
    }

    if ($needDist) {
        Write-Host '[MOSS Dev] Building frontend/dist once so packaged index.html is present...' -ForegroundColor Cyan
        Push-Location $frontendDir
        try {
            & $npmCmd run build
            if ($LASTEXITCODE -ne 0) { throw 'Initial frontend build failed.' }
        } finally {
            Pop-Location
        }
    }

    if (Test-LoopbackPort $devHost $devPort) {
        if (-not (Test-VerifiedMossViteOrigin $devOrigin $devPort $frontendDir)) {
            throw "Port $devPort is already in use by an unverified or non-MOSS server. Stop that listener before running dev-desktop.ps1."
        }
        Write-Host "[MOSS Dev] Reusing verified Vite server at $devOrigin" -ForegroundColor Green
    } else {
        Write-Host "[MOSS Dev] Starting Vite dev server at $devOrigin ..." -ForegroundColor Cyan
        $viteProc = Start-Process -FilePath $npmCmd -ArgumentList @('run', 'dev') -WorkingDirectory $frontendDir -PassThru -NoNewWindow
        $ready = $false
        for ($i = 0; $i -lt 80; $i++) {
            if ($viteProc.HasExited) {
                throw "Vite dev server exited early with code $($viteProc.ExitCode)."
            }
            if ((Test-LoopbackPort $devHost $devPort) -and (Test-VerifiedMossViteOrigin $devOrigin $devPort $frontendDir)) {
                $ready = $true
                break
            }
            Start-Sleep -Milliseconds 250
        }
        if (-not $ready) {
            throw "Timed out waiting for verified Vite dev server on $devOrigin."
        }
    }

    $env:MOSS_DEV_UI_ORIGIN = $devOrigin
    Write-Host "[MOSS Dev] Launching MOSS native desktop window v$effectiveVersion with hot reload ($devOrigin)..." -ForegroundColor Green
    Push-Location $repo
    try {
        & cargo run -p moss-desktop
        if ($LASTEXITCODE -ne 0) { throw "moss-desktop exited with code $LASTEXITCODE." }
    } finally {
        Pop-Location
    }
} finally {
    if ($null -eq $prevDevOrigin) {
        Remove-Item Env:MOSS_DEV_UI_ORIGIN -ErrorAction SilentlyContinue
    } else {
        $env:MOSS_DEV_UI_ORIGIN = $prevDevOrigin
    }
    if ($null -eq $prevReleaseVer) {
        Remove-Item Env:MOSS_RELEASE_VERSION -ErrorAction SilentlyContinue
    } else {
        $env:MOSS_RELEASE_VERSION = $prevReleaseVer
    }
    if ($null -eq $prevTauriCfg) {
        Remove-Item Env:TAURI_CONFIG -ErrorAction SilentlyContinue
    } else {
        $env:TAURI_CONFIG = $prevTauriCfg
    }
    if ($null -ne $viteProc -and -not $viteProc.HasExited) {
        Write-Host '[MOSS Dev] Stopping Vite dev server...' -ForegroundColor Yellow
        if ($env:OS -eq 'Windows_NT') {
            & taskkill.exe /PID $viteProc.Id /T /F *> $null
        } else {
            Stop-Process -Id $viteProc.Id -Force -ErrorAction SilentlyContinue
        }
    }
}
