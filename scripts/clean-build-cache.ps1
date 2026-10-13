# Removes rebuildable build caches. Usage: powershell -File scripts/clean-build-cache.ps1 [-WhatIf]
# Deletes: incremental dirs, target/tmp and codex-*, stray sub-crate target/ dirs, Android Gradle
#          intermediates and debug APKs, older NV18 staging copies, and stale hashed artifacts of
#          this repo's own crates in deps/ (newest kept per kind).
# Keeps: third-party artifacts, build/ dirs, delivery outputs, node_modules, sources and .runtime/.
[CmdletBinding(SupportsShouldProcess = $true)]
param()

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$target = Join-Path $repo 'target'
$script:freed = 0L

function Get-Size([string]$path) {
    if (Test-Path -LiteralPath $path -PathType Leaf) { return (Get-Item -LiteralPath $path).Length }
    $sum = (Get-ChildItem -LiteralPath $path -Recurse -File -Force -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
    if ($sum) { return [long]$sum } else { return 0L }
}

function Remove-Cache([string]$path) {
    if (-not (Test-Path -LiteralPath $path)) { return }
    $size = Get-Size $path
    if ($PSCmdlet.ShouldProcess($path, ('delete {0:N1} MB' -f ($size / 1MB)))) {
        Remove-Item -LiteralPath $path -Recurse -Force
    }
    $script:freed += $size
}

if (-not (Test-Path -LiteralPath $target)) { Write-Host 'No target/ directory; nothing to clean.'; exit 0 }

# 1. Incremental caches and temporary check dirs
Get-ChildItem -LiteralPath $target -Directory -Recurse -Depth 2 -Filter incremental | ForEach-Object { Remove-Cache $_.FullName }
Get-ChildItem -LiteralPath $target -Directory | Where-Object { $_.Name -eq 'tmp' -or $_.Name -like 'codex-*' } |
    ForEach-Object { Remove-Cache $_.FullName }

# 2. Stray target/ dirs (workspace and helper builds use the root target/)
foreach ($pattern in 'crates/*/target', 'apps/*/target', 'apps/desktop/src-tauri/target', 'tools/*/target') {
    Get-ChildItem -Path (Join-Path $repo $pattern) -Directory -ErrorAction SilentlyContinue | ForEach-Object { Remove-Cache $_.FullName }
}

# 3. Android Gradle intermediates and debug APKs (release APK kept)
$android = Join-Path $repo 'apps/desktop/src-tauri/gen/android/app/build'
Remove-Cache (Join-Path $android 'intermediates')
Get-ChildItem -Path (Join-Path $android 'outputs/apk/*/debug') -Directory -ErrorAction SilentlyContinue | ForEach-Object { Remove-Cache $_.FullName }

# 4. NV18 staging copies: keep the newest (originals live in apps/moss-voice-local)
foreach ($staging in 'apps/desktop/src-tauri/bundle-resources', 'apps/desktop/src-tauri/binaries') {
    Get-ChildItem -Path (Join-Path $repo $staging) -Directory -Filter 'nv18-*' -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -Skip 1 | ForEach-Object { Remove-Cache $_.FullName }
}

# 5. Stale hashed artifacts of this repo's crates in deps/
$names = New-Object System.Collections.Generic.HashSet[string]
$manifests = @(Get-ChildItem -Path (Join-Path $repo 'crates/*/Cargo.toml')) +
    @(Get-ChildItem -Path (Join-Path $repo 'apps/*/Cargo.toml')) +
    @(Get-Item -LiteralPath (Join-Path $repo 'apps/desktop/src-tauri/Cargo.toml'))
foreach ($m in $manifests) {
    foreach ($match in [regex]::Matches((Get-Content -LiteralPath $m.FullName -Raw), '(?m)^name\s*=\s*"([^"]+)"')) {
        [void]$names.Add($match.Groups[1].Value.Replace('-', '_'))
    }
    $tests = Join-Path $m.DirectoryName 'tests'
    if (Test-Path -LiteralPath $tests) {
        Get-ChildItem -LiteralPath $tests -Filter *.rs -File | ForEach-Object { [void]$names.Add($_.BaseName) }
    }
}

$artifact = '^(lib)?(.+)-([0-9a-f]{16})(\..+)?$'
Get-ChildItem -LiteralPath $target -Directory -Recurse -Depth 2 -Filter deps | ForEach-Object {
    Get-ChildItem -LiteralPath $_.FullName -File |
        Where-Object { $_.Name -match $artifact -and $names.Contains($Matches[2]) } |
        Group-Object { $null = $_.Name -match $artifact; '{0}|{1}|{2}' -f $Matches[1], $Matches[2], $Matches[4] } |
        ForEach-Object { $_.Group | Sort-Object LastWriteTime -Descending | Select-Object -Skip 1 | ForEach-Object { Remove-Cache $_.FullName } }
}

Write-Host ('Freed (or would free with -WhatIf): {0:N2} GB' -f ($script:freed / 1GB))
