# Fast official release build of moss-desktop.exe (--no-bundle) and optional in-place sync
# to an existing local MOSS installation without repacking the full NSIS installer.
# Never force-kills running MOSS processes (preserves I-7 emergency-stop shutdown order).
# Usage:
#   powershell -File scripts/quick-update-desktop.ps1 [-BuildOnly] [-InstallDir <dir>]
#       [-Version <ver>] [-WithVoiceHelper] [-WithHost] [-Launch] [-SkipCacheSweep] [-WhatIf]
# Pure ASCII script for Windows PowerShell 5.1 compatibility.
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [switch]$BuildOnly,
    [string]$InstallDir = '',
    [string]$Version = '',
    [switch]$WithVoiceHelper,
    [switch]$WithHost,
    [switch]$Launch,
    [switch]$SkipCacheSweep
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$frontendDir = Join-Path $repo 'frontend'
$tauriDir = Join-Path $repo 'apps/desktop/src-tauri'
$releaseExe = Join-Path $repo 'target/release/moss-desktop.exe'
$hostExe = Join-Path $repo 'target/release/moss-host.exe'
$triple = 'x86_64-pc-windows-msvc'
$protoSource = Join-Path $repo 'crates/moss-voice-local-proto/src/lib.rs'
$helperManifest = Join-Path $repo 'apps/moss-voice-local/Cargo.toml'
$helperExe = Join-Path $repo "target/$triple/release/moss-voice-local.exe"
$releaseHelperExe = Join-Path $repo 'target/release/moss-voice-local.exe'
$debugHelperExe = Join-Path $repo 'target/debug/moss-voice-local.exe'
$sidecarExe = Join-Path $tauriDir "binaries/moss-voice-local-$triple.exe"
$npmCmd = if ($env:OS -eq 'Windows_NT') { 'npm.cmd' } else { 'npm' }

function Assert-MossNotRunning([string[]]$names) {
    $running = @(Get-Process -Name $names -ErrorAction SilentlyContinue)
    if ($running.Count -gt 0) {
        $list = ($running | Select-Object -ExpandProperty ProcessName -Unique) -join ', '
        throw "MOSS process is currently running ($list). Please exit MOSS from the tray first (never force-killed, to preserve 0x18 emergency-stop order), then re-run."
    }
}

function Resolve-ExistingMossInstallDir([string]$explicitDir) {
    if ($explicitDir -ne '') {
        if (-not (Test-Path -LiteralPath $explicitDir -PathType Container)) {
            throw "Specified -InstallDir does not exist: $explicitDir"
        }
        $resolved = (Resolve-Path -LiteralPath $explicitDir).Path
        $candidateExe = Join-Path $resolved 'moss-desktop.exe'
        if (-not (Test-Path -LiteralPath $candidateExe -PathType Leaf)) {
            throw "Refusing to sync into '$resolved': moss-desktop.exe not found there."
        }
        return $resolved
    }

    if ($env:OS -ne 'Windows_NT') {
        return $null
    }

    $targetReleaseDir = Join-Path $repo 'target\release'
    $targetDebugDir = Join-Path $repo 'target\debug'
    $candidates = New-Object System.Collections.Generic.List[string]

    $regRoots = @(
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\Software\WOW6432Node\Software\Microsoft\Windows\CurrentVersion\Uninstall\*'
    )
    foreach ($regPath in $regRoots) {
        $items = @(Get-ItemProperty -Path $regPath -ErrorAction SilentlyContinue | Where-Object {
            ($_.PSObject.Properties['DisplayName'] -and $_.DisplayName -eq 'MOSS') -or
            $_.PSChildName -eq 'MOSS' -or
            $_.PSChildName -eq 'com.moss.laser'
        })
        foreach ($item in $items) {
            if ($item.PSObject.Properties['InstallLocation'] -and $item.InstallLocation) {
                $candidates.Add([string]$item.InstallLocation)
            }
            if ($item.PSObject.Properties['DisplayIcon'] -and $item.DisplayIcon) {
                $iconPath = ([string]$item.DisplayIcon).Trim('"') -replace ',[0-9-]+$', ''
                $parent = Split-Path -Parent $iconPath
                if ($parent) { $candidates.Add($parent) }
            }
            if ($item.PSObject.Properties['UninstallString'] -and $item.UninstallString) {
                $uninst = [string]$item.UninstallString
                if ($uninst -match '^"([^"]+)"') { $uninst = $Matches[1] } else { $uninst = ($uninst -split '\s+')[0] }
                $parent = Split-Path -Parent $uninst
                if ($parent) { $candidates.Add($parent) }
            }
        }
    }

    if ($env:LOCALAPPDATA) {
        $candidates.Add((Join-Path $env:LOCALAPPDATA 'MOSS'))
    }
    if ($env:ProgramFiles) {
        $candidates.Add((Join-Path $env:ProgramFiles 'MOSS'))
    }

    foreach ($cand in $candidates) {
        if ([string]::IsNullOrWhiteSpace($cand)) { continue }
        $trimmed = $cand.Trim().Trim('"')
        if (-not (Test-Path -LiteralPath $trimmed -PathType Container)) { continue }
        $full = (Resolve-Path -LiteralPath $trimmed).Path
        if ($full -ieq $targetReleaseDir -or $full -ieq $targetDebugDir) { continue }
        if (Test-Path -LiteralPath (Join-Path $full 'moss-desktop.exe') -PathType Leaf) {
            return $full
        }
    }

    return $null
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

function Clear-StaleRenamedBinaries([string]$dir) {
    if (-not (Test-Path -LiteralPath $dir -PathType Container)) { return }
    Get-ChildItem -LiteralPath $dir -Filter '*.old-*.exe' -File -ErrorAction SilentlyContinue | ForEach-Object {
        if (-not (Test-FileLocked $_.FullName)) {
            Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue
        }
    }
}

function Move-AsideIfLocked([string]$exePath) {
    if (-not (Test-FileLocked $exePath)) { return $false }
    $dir = Split-Path -Parent $exePath
    $stem = [System.IO.Path]::GetFileNameWithoutExtension($exePath)
    $stamp = (Get-Date).ToString('yyyyMMddHHmmss')
    $aside = Join-Path $dir "${stem}.old-${stamp}.exe"
    Move-Item -LiteralPath $exePath -Destination $aside -Force
    Write-Host "[MOSS QuickUpdate] Active binary is open; moved running file aside -> $(Split-Path -Leaf $aside) without interrupting MOSS." -ForegroundColor Yellow
    return $true
}

function Get-ExpectedVoiceHelperProtocol {
    if (-not (Test-Path -LiteralPath $protoSource -PathType Leaf)) {
        throw "Missing voice helper protocol source: $protoSource"
    }
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

function Sync-HelperBinary([string]$srcPath, [string]$dstPath) {
    if ($srcPath -ieq $dstPath) { return $false }
    $dstParent = Split-Path -Parent $dstPath
    New-Item -ItemType Directory -Path $dstParent -Force | Out-Null
    $srcHash = (Get-FileHash -LiteralPath $srcPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if (Test-Path -LiteralPath $dstPath -PathType Leaf) {
        try {
            $existingHash = (Get-FileHash -LiteralPath $dstPath -Algorithm SHA256).Hash.ToLowerInvariant()
        } catch {
            $existingHash = ''
        }
        if ($srcHash -eq $existingHash) {
            return $false
        }
    }
    Clear-StaleRenamedBinaries $dstParent
    if (Move-AsideIfLocked $dstPath) { $script:movedRunningAside = $true }
    Copy-Item -LiteralPath $srcPath -Destination $dstPath -Force
    $dstHash = (Get-FileHash -LiteralPath $dstPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($srcHash -ne $dstHash) {
        throw "SHA-256 mismatch after syncing voice helper to $dstPath"
    }
    return $true
}

$syncTargetDir = $null
if (-not $BuildOnly) {
    $syncTargetDir = Resolve-ExistingMossInstallDir $InstallDir
}

$movedRunningAside = $false
if (-not $WhatIfPreference) {
    Clear-StaleRenamedBinaries (Split-Path -Parent $releaseExe)
    if (Move-AsideIfLocked $releaseExe) { $movedRunningAside = $true }
}

$expectedProto = Get-ExpectedVoiceHelperProtocol
$tauriConfPath = Join-Path $tauriDir 'tauri.conf.json'
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

function Resolve-EffectiveMossVersion([string]$explicitVer, [string]$defaultVer) {
    $clean = $explicitVer.Trim()
    if ($clean.StartsWith('v')) { $clean = $clean.Substring(1) }
    if ($clean -ne '') {
        if ($clean -notmatch '^\d+\.\d+\.\d+([.-][0-9A-Za-z.-]+)?$') {
            throw "Invalid -Version '$explicitVer' (expected semantic version X.Y.Z)."
        }
        return $clean
    }
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

$effectiveVersion = Resolve-EffectiveMossVersion $Version $confVersion
$activeHelperSource = $null
$startedAt = (Get-Date).AddSeconds(-2)

if ($PSCmdlet.ShouldProcess($releaseExe, "Build official release moss-desktop.exe v$effectiveVersion (--no-bundle)")) {
    $prevReleaseVer = $env:MOSS_RELEASE_VERSION
    $prevTauriCfg = $env:TAURI_CONFIG
    $env:MOSS_RELEASE_VERSION = $effectiveVersion
    if ($effectiveVersion -ne $confVersion) {
        $env:TAURI_CONFIG = (@{ version = $effectiveVersion } | ConvertTo-Json -Compress)
    }
    try {
    Write-Host "[MOSS QuickUpdate] Embedding release version $effectiveVersion ..." -ForegroundColor Cyan
    if (-not $WithVoiceHelper) {
        foreach ($cand in @($helperExe, $sidecarExe, $releaseHelperExe)) {
            if ((Get-BinaryVoiceHelperProtocol $cand) -eq $expectedProto) {
                $activeHelperSource = $cand
                break
            }
        }
    }

    if ($WithVoiceHelper -or $null -eq $activeHelperSource) {
        if (Move-AsideIfLocked $helperExe) { $movedRunningAside = $true }
        Write-Host "[MOSS QuickUpdate] Building release moss-voice-local.exe (protocol $expectedProto) ..." -ForegroundColor Cyan
        & cargo build --release --manifest-path $helperManifest --target $triple --target-dir (Join-Path $repo 'target')
        if ($LASTEXITCODE -ne 0) { throw 'moss-voice-local release build failed.' }
        if (-not (Test-Path -LiteralPath $helperExe -PathType Leaf)) {
            throw "Missing built helper binary: $helperExe"
        }
        if ((Get-BinaryVoiceHelperProtocol $helperExe) -ne $expectedProto) {
            throw "Built helper binary protocol mismatch at $helperExe (expected protocol $expectedProto)."
        }
        $activeHelperSource = $helperExe
    }

    [void](Sync-HelperBinary $activeHelperSource $sidecarExe)

    $hasCargoTauri = $false
    try {
        & cargo tauri --version *> $null
        if ($LASTEXITCODE -eq 0) { $hasCargoTauri = $true }
    } catch {
        $hasCargoTauri = $false
    }

    if ($hasCargoTauri) {
        Write-Host '[MOSS QuickUpdate] Running cargo tauri build --no-bundle (builds frontend/dist + release moss-desktop.exe, skips NSIS installer)...' -ForegroundColor Cyan
        $versionOverlay = $null
        if ($effectiveVersion -ne $confVersion) {
            $versionOverlay = Join-Path ([System.IO.Path]::GetTempPath()) "moss-tauri-ver-$PID.json"
            $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
            [System.IO.File]::WriteAllText($versionOverlay, (@{ version = $effectiveVersion } | ConvertTo-Json -Compress), $utf8NoBom)
        }
        Push-Location $tauriDir
        try {
            if ($null -ne $versionOverlay) {
                & cargo tauri build --no-bundle --config $versionOverlay
            } else {
                & cargo tauri build --no-bundle
            }
            if ($LASTEXITCODE -ne 0) {
                if (Move-AsideIfLocked $releaseExe) {
                    $movedRunningAside = $true
                    Pop-Location
                    Push-Location $repo
                    & cargo build --release -p moss-desktop --features tauri/custom-protocol
                }
                if ($LASTEXITCODE -ne 0) { throw 'cargo tauri build --no-bundle failed.' }
            }
        } finally {
            Pop-Location
            if ($null -ne $versionOverlay -and (Test-Path -LiteralPath $versionOverlay -PathType Leaf)) {
                Remove-Item -LiteralPath $versionOverlay -Force -ErrorAction SilentlyContinue
            }
        }
    } else {
        Write-Host '[MOSS QuickUpdate] cargo-tauri not found; building frontend/dist + cargo build --release -p moss-desktop --features tauri/custom-protocol ...' -ForegroundColor Cyan
        Push-Location $frontendDir
        try {
            & $npmCmd run build
            if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed.' }
        } finally {
            Pop-Location
        }
        if (Move-AsideIfLocked $releaseExe) { $movedRunningAside = $true }
        Push-Location $repo
        try {
            & cargo build --release -p moss-desktop --features tauri/custom-protocol
            if ($LASTEXITCODE -ne 0) { throw 'moss-desktop release build failed.' }
        } finally {
            Pop-Location
        }
    }

    if ($WithHost) {
        if (Move-AsideIfLocked $hostExe) { $movedRunningAside = $true }
        Write-Host '[MOSS QuickUpdate] Building release moss-host.exe ...' -ForegroundColor Cyan
        Push-Location $repo
        try {
            & cargo build --release -p moss-host
            if ($LASTEXITCODE -ne 0) { throw 'moss-host release build failed.' }
        } finally {
            Pop-Location
        }
    }

    if (-not (Test-Path -LiteralPath $releaseExe -PathType Leaf)) {
        throw "Release executable missing after build: $releaseExe"
    }
    $exeItem = Get-Item -LiteralPath $releaseExe
    if ($exeItem.Length -le 0) {
        throw "Release executable is empty: $releaseExe"
    }
    if ($exeItem.LastWriteTime -lt $startedAt) {
        Write-Host "[MOSS QuickUpdate] Note: Cargo reused up-to-date release binary (no source changes detected since $($exeItem.LastWriteTime.ToString('s')))." -ForegroundColor Yellow
    }
    $srcHash = (Get-FileHash -LiteralPath $releaseExe -Algorithm SHA256).Hash.ToLowerInvariant()
    $sizeMb = [math]::Round($exeItem.Length / 1MB, 2)
    Write-Host ("[MOSS QuickUpdate] Built release moss-desktop.exe ({0} MB, sha256={1})" -f $sizeMb, $srcHash) -ForegroundColor Green

    if (Sync-HelperBinary $activeHelperSource $releaseHelperExe) {
        Write-Host "[MOSS QuickUpdate] Synced voice helper (protocol $expectedProto) -> $releaseHelperExe" -ForegroundColor Green
    }
    if (Test-Path -LiteralPath (Split-Path -Parent $debugHelperExe) -PathType Container) {
        if (Sync-HelperBinary $activeHelperSource $debugHelperExe) {
            Write-Host "[MOSS QuickUpdate] Synced voice helper (protocol $expectedProto) -> $debugHelperExe" -ForegroundColor Green
        }
    }
    } finally {
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
    }
}

$launchTarget = $releaseExe

if (-not $BuildOnly) {
    if ($null -eq $syncTargetDir) {
        Write-Host '[MOSS QuickUpdate] No existing NSIS install folder detected; fresh release binary is ready at target/release/moss-desktop.exe (launch via scripts\start.bat).' -ForegroundColor Yellow
    } else {
        $destExe = Join-Path $syncTargetDir 'moss-desktop.exe'
        if ($PSCmdlet.ShouldProcess($destExe, 'Replace installed moss-desktop.exe in place (keeps %APPDATA%\moss data intact)')) {
            Clear-StaleRenamedBinaries $syncTargetDir
            if (Move-AsideIfLocked $destExe) { $movedRunningAside = $true }
            Copy-Item -LiteralPath $releaseExe -Destination $destExe -Force
            $srcHash = (Get-FileHash -LiteralPath $releaseExe -Algorithm SHA256).Hash.ToLowerInvariant()
            $dstHash = (Get-FileHash -LiteralPath $destExe -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($srcHash -ne $dstHash) {
                throw "SHA-256 mismatch after copying to $destExe"
            }
            Write-Host "[MOSS QuickUpdate] Synced official release binary -> $destExe (sha256=$dstHash)" -ForegroundColor Green

            $destHelper = Join-Path $syncTargetDir 'moss-voice-local.exe'
            if ($null -ne $activeHelperSource -and ($WithVoiceHelper -or (Test-Path -LiteralPath $destHelper -PathType Leaf))) {
                if (Sync-HelperBinary $activeHelperSource $destHelper) {
                    Write-Host "[MOSS QuickUpdate] Synced release voice helper (protocol $expectedProto) -> $destHelper" -ForegroundColor Green
                }
            }
            if ($WithHost -and (Test-Path -LiteralPath (Join-Path $syncTargetDir 'moss-host.exe') -PathType Leaf)) {
                $destHost = Join-Path $syncTargetDir 'moss-host.exe'
                if (Move-AsideIfLocked $destHost) { $movedRunningAside = $true }
                Copy-Item -LiteralPath $hostExe -Destination $destHost -Force
                Write-Host "[MOSS QuickUpdate] Synced release moss-host -> $destHost" -ForegroundColor Green
            }
        }
        $launchTarget = $destExe
    }
}

if (-not $SkipCacheSweep -and -not $WhatIfPreference) {
    if (Get-Command cargo-sweep.exe -CommandType Application -ErrorAction SilentlyContinue) {
        Push-Location $repo
        try {
            & cargo sweep --time 3 *> $null
        } finally {
            Pop-Location
        }
    }
}

if ($movedRunningAside) {
    Write-Host '[MOSS QuickUpdate] MOSS is currently open (left untouched); close and reopen MOSS to use the newly updated release binary.' -ForegroundColor Cyan
}

if ($Launch -and -not $WhatIfPreference) {
    Assert-MossNotRunning @('moss-desktop')
    Write-Host "[MOSS QuickUpdate] Launching $launchTarget ..." -ForegroundColor Green
    Start-Process -FilePath $launchTarget
}
