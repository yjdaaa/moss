# Removes rebuildable build caches. Usage: powershell -File scripts/clean-build-cache.ps1 [-WhatIf]
# Deletes: incremental dirs, target/tmp and codex-*, stray sub-crate target/ dirs, Android Gradle
#          intermediates and debug APKs, older NV18 staging copies, and stale hashed artifacts of
#          this repo's own crates in deps/ (newest kept per kind).
# Keeps: third-party artifacts, build/ dirs, delivery outputs, node_modules, sources and .runtime/.
[CmdletBinding(SupportsShouldProcess = $true)]
param()

$ErrorActionPreference = 'Stop'
$repo = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$target = Join-Path $repo 'target'
$script:freed = 0L
$script:removed = New-Object System.Collections.Generic.HashSet[string] ([System.StringComparer]::OrdinalIgnoreCase)

function Test-ReparsePoint($item) {
    return ($null -ne $item) -and $item.Attributes.HasFlag([System.IO.FileAttributes]::ReparsePoint)
}

function Test-HasReparseAncestor([string]$path) {
    $full = [System.IO.Path]::GetFullPath($path).TrimEnd('\', '/')
    $root = $repo.TrimEnd('\', '/')
    if ($full -ieq $root) { return $false }
    if (-not $full.StartsWith($root + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $true
    }
    $rel = $full.Substring($root.Length + 1)
    $cur = $root
    foreach ($part in ($rel -split '[\\/]+')) {
        if ([string]::IsNullOrEmpty($part)) { continue }
        $cur = Join-Path $cur $part
        $item = Get-Item -LiteralPath $cur -Force -ErrorAction SilentlyContinue
        if (Test-ReparsePoint $item) { return $true }
    }
    return $false
}

function Test-AlreadyRemoved([string]$path) {
    $cur = [System.IO.Path]::GetFullPath($path).TrimEnd('\', '/')
    while (-not [string]::IsNullOrEmpty($cur)) {
        if ($script:removed.Contains($cur)) { return $true }
        $parent = Split-Path -Parent $cur
        if ($parent -eq $cur) { break }
        $cur = $parent
    }
    return $false
}

function Get-Size([string]$path) {
    $item = Get-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
    if ($null -eq $item -or (Test-ReparsePoint $item)) { return 0L }
    if (-not $item.PSIsContainer) { return [long]$item.Length }
    $total = 0L
    $stack = New-Object System.Collections.Generic.Stack[System.IO.DirectoryInfo]
    $stack.Push([System.IO.DirectoryInfo]$item)
    while ($stack.Count -gt 0) {
        $dir = $stack.Pop()
        try {
            foreach ($f in $dir.GetFiles()) {
                if (-not (Test-ReparsePoint $f)) { $total += $f.Length }
            }
            foreach ($sub in $dir.GetDirectories()) {
                if (-not (Test-ReparsePoint $sub)) { $stack.Push($sub) }
            }
        } catch {
            # Ignore unreadable entries during cache sizing
        }
    }
    return $total
}

function Remove-TreeSafe([string]$path) {
    $item = Get-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
    if ($null -eq $item) { return }
    if (Test-ReparsePoint $item) {
        try {
            if ($item.PSIsContainer) { [System.IO.Directory]::Delete($item.FullName, $false) }
            else { [System.IO.File]::Delete($item.FullName) }
        } catch {}
        return
    }
    if (-not $item.PSIsContainer) {
        try {
            if ($item.IsReadOnly) { $item.IsReadOnly = $false }
            [System.IO.File]::Delete($item.FullName)
        } catch {}
        return
    }
    $dir = [System.IO.DirectoryInfo]$item
    $files = @()
    $subs = @()
    try { $files = $dir.GetFiles() } catch {}
    try { $subs = $dir.GetDirectories() } catch {}
    foreach ($f in $files) {
        try {
            if ((-not (Test-ReparsePoint $f)) -and $f.IsReadOnly) { $f.IsReadOnly = $false }
            [System.IO.File]::Delete($f.FullName)
        } catch {}
    }
    foreach ($sub in $subs) {
        if (Test-ReparsePoint $sub) {
            try { [System.IO.Directory]::Delete($sub.FullName, $false) } catch {}
        } else {
            Remove-TreeSafe $sub.FullName
        }
    }
    try {
        if ($dir.Attributes.HasFlag([System.IO.FileAttributes]::ReadOnly)) {
            $dir.Attributes = ($dir.Attributes -band (-bnot [System.IO.FileAttributes]::ReadOnly))
        }
        [System.IO.Directory]::Delete($dir.FullName, $false)
    } catch {}
}

function Find-NamedDirs([string]$rootDir, [string]$targetName, [int]$maxDepth) {
    $results = New-Object System.Collections.Generic.List[System.IO.DirectoryInfo]
    if (-not (Test-Path -LiteralPath $rootDir) -or (Test-HasReparseAncestor $rootDir)) { return $results }
    $queue = New-Object System.Collections.Generic.Queue[object]
    $queue.Enqueue(@{ Dir = [System.IO.DirectoryInfo](Get-Item -LiteralPath $rootDir -Force); Depth = 0 })
    while ($queue.Count -gt 0) {
        $entry = $queue.Dequeue()
        if ($entry.Depth -gt $maxDepth) { continue }
        try {
            foreach ($sub in $entry.Dir.GetDirectories()) {
                if (Test-ReparsePoint $sub) { continue }
                if ($sub.Name -ieq $targetName) {
                    $results.Add($sub)
                } elseif ($entry.Depth -lt $maxDepth) {
                    $queue.Enqueue(@{ Dir = $sub; Depth = ($entry.Depth + 1) })
                }
            }
        } catch {}
    }
    return $results
}

function Remove-Cache([string]$path) {
    $full = [System.IO.Path]::GetFullPath($path).TrimEnd('\', '/')
    if ((Test-AlreadyRemoved $full) -or (-not (Test-Path -LiteralPath $full)) -or (Test-HasReparseAncestor $full)) { return }
    $size = Get-Size $full
    [void]$script:removed.Add($full)
    if ($PSCmdlet.ShouldProcess($full, ('delete {0:N1} MB' -f ($size / 1MB)))) {
        Remove-TreeSafe $full
    }
    $script:freed += $size
}

if (-not (Test-Path -LiteralPath $target) -or (Test-HasReparseAncestor $target)) { Write-Host 'No target/ directory; nothing to clean.'; exit 0 }

# 0. Clean build artifacts older than 3 days via cargo-sweep (root + independent helper workspace)
if (Get-Command cargo-sweep.exe -CommandType Application -ErrorAction SilentlyContinue) {
    foreach ($ws in @($repo, (Join-Path $repo 'apps/moss-voice-local'))) {
        $wsTarget = Join-Path $ws 'target'
        if (-not (Test-Path -LiteralPath $wsTarget) -or (Test-HasReparseAncestor $wsTarget)) { continue }
        Push-Location $ws
        try {
            if ($WhatIfPreference) {
                & cargo sweep --time 3 --dry-run
            } else {
                & cargo sweep --time 3
            }
        } finally {
            Pop-Location
        }
    }
}

# 1. Incremental caches: remove >3 days old, and keep only the newest session dir per crate prefix
$cutoff = (Get-Date).AddDays(-3)
foreach ($inc in (Find-NamedDirs $target 'incremental' 3)) {
    $incDirs = @(Get-ChildItem -LiteralPath $inc.FullName -Directory -ErrorAction SilentlyContinue | Where-Object { -not (Test-ReparsePoint $_) })
    $incDirs | Where-Object { $_.LastWriteTime -lt $cutoff } | ForEach-Object { Remove-Cache $_.FullName }
    $incDirs = @(Get-ChildItem -LiteralPath $inc.FullName -Directory -ErrorAction SilentlyContinue | Where-Object { (-not (Test-ReparsePoint $_)) -and (-not (Test-AlreadyRemoved $_.FullName)) })
    $incDirs | Group-Object { if ($_.Name -match '^(.+)-[0-9a-z]+$') { $Matches[1] } else { $_.Name } } | ForEach-Object {
        $_.Group | Sort-Object LastWriteTime -Descending | Select-Object -Skip 1 | ForEach-Object { Remove-Cache $_.FullName }
        $newest = $_.Group | Sort-Object LastWriteTime -Descending | Select-Object -First 1
        if ($newest -and (-not (Test-AlreadyRemoved $newest.FullName)) -and (Test-Path -LiteralPath $newest.FullName)) {
            Get-ChildItem -LiteralPath $newest.FullName -Directory -ErrorAction SilentlyContinue |
                Where-Object { -not (Test-ReparsePoint $_) } |
                Sort-Object LastWriteTime -Descending | Select-Object -Skip 1 |
                ForEach-Object { Remove-Cache $_.FullName }
        }
    }
}
Get-ChildItem -LiteralPath $target -Directory | Where-Object { (-not (Test-ReparsePoint $_)) -and ($_.Name -eq 'tmp' -or $_.Name -like 'codex-*') } |
    ForEach-Object { Remove-Cache $_.FullName }

# 2. Stray target/ dirs (exempt independent workspace apps/moss-voice-local/target)
$independentTarget = [System.IO.Path]::GetFullPath((Join-Path $repo 'apps/moss-voice-local/target')).TrimEnd('\', '/')
foreach ($pattern in 'crates/*/target', 'apps/*/target', 'apps/desktop/src-tauri/target', 'tools/*/target') {
    Get-Item -Path (Join-Path $repo $pattern) -ErrorAction SilentlyContinue |
        Where-Object { $_.PSIsContainer -and (-not (Test-HasReparseAncestor $_.FullName)) -and ([System.IO.Path]::GetFullPath($_.FullName).TrimEnd('\', '/') -ine $independentTarget) } |
        ForEach-Object { Remove-Cache $_.FullName }
}

# 3. Android Gradle intermediates and debug APKs (release APK kept)
$android = Join-Path $repo 'apps/desktop/src-tauri/gen/android/app/build'
Remove-Cache (Join-Path $android 'intermediates')
Get-Item -Path (Join-Path $android 'outputs/apk/*/debug') -ErrorAction SilentlyContinue |
    Where-Object { $_.PSIsContainer -and (-not (Test-HasReparseAncestor $_.FullName)) } |
    ForEach-Object { Remove-Cache $_.FullName }

# 4. NV18 staging copies & temporary license staging (originals live in apps/moss-voice-local)
foreach ($staging in 'apps/desktop/src-tauri/bundle-resources', 'apps/desktop/src-tauri/binaries') {
    $stagingPath = Join-Path $repo $staging
    if ((Test-Path -LiteralPath $stagingPath) -and (-not (Test-HasReparseAncestor $stagingPath))) {
        Get-ChildItem -LiteralPath $stagingPath -Directory -Filter 'nv18-*' -ErrorAction SilentlyContinue |
            Where-Object { -not (Test-ReparsePoint $_) } |
            ForEach-Object { Remove-Cache $_.FullName }
    }
}
Get-ChildItem -LiteralPath $target -Directory -Filter 'nv18-*' -ErrorAction SilentlyContinue |
    Where-Object { -not (Test-ReparsePoint $_) } |
    Sort-Object LastWriteTime -Descending | Select-Object -Skip 1 | ForEach-Object { Remove-Cache $_.FullName }
Get-Item -Path (Join-Path $target '*/release/licenses') -ErrorAction SilentlyContinue |
    Where-Object { $_.PSIsContainer -and (-not (Test-HasReparseAncestor $_.FullName)) } |
    ForEach-Object { Remove-Cache $_.FullName }
Get-Item -Path (Join-Path $target 'release/*.old-*.exe') -ErrorAction SilentlyContinue |
    Where-Object { (-not $_.PSIsContainer) -and (-not (Test-HasReparseAncestor $_.FullName)) } |
    ForEach-Object { Remove-Cache $_.FullName }
foreach ($nsisDir in @(Join-Path $target 'release/bundle/nsis'), (Join-Path $target 'x86_64-pc-windows-msvc/release/bundle/nsis')) {
    if ((Test-Path -LiteralPath $nsisDir) -and (-not (Test-HasReparseAncestor $nsisDir))) {
        Get-ChildItem -LiteralPath $nsisDir -Filter '*.exe' -File -ErrorAction SilentlyContinue |
            Where-Object { -not (Test-ReparsePoint $_) } |
            Sort-Object LastWriteTime -Descending | Select-Object -Skip 1 |
            ForEach-Object { Remove-Cache $_.FullName }
    }
}

# 5. Stale hashed artifacts of this repo's crates in deps/ (newest kept per kind)
$crateNames = New-Object System.Collections.Generic.HashSet[string]
$testNames = New-Object System.Collections.Generic.HashSet[string]
$manifests = @(Get-ChildItem -Path (Join-Path $repo 'crates/*/Cargo.toml')) +
    @(Get-ChildItem -Path (Join-Path $repo 'apps/*/Cargo.toml')) +
    @(Get-Item -LiteralPath (Join-Path $repo 'apps/desktop/src-tauri/Cargo.toml'))
foreach ($m in $manifests) {
    if (Test-HasReparseAncestor $m.FullName) { continue }
    foreach ($match in [regex]::Matches((Get-Content -LiteralPath $m.FullName -Raw), '(?m)^name\s*=\s*"([^"]+)"')) {
        $crateName = $match.Groups[1].Value.Replace('-', '_')
        [void]$crateNames.Add($crateName)
        [void]$crateNames.Add("${crateName}_lib")
    }
    $tests = Join-Path $m.DirectoryName 'tests'
    if ((Test-Path -LiteralPath $tests) -and (-not (Test-HasReparseAncestor $tests))) {
        Get-ChildItem -LiteralPath $tests -Filter *.rs -File | Where-Object { -not (Test-ReparsePoint $_) } |
            ForEach-Object { [void]$testNames.Add($_.BaseName.Replace('-', '_')) }
    }
}

$artifact = '^(lib)?(.+)-([0-9a-f]{16})(\..+)?$'
foreach ($depsDir in (Find-NamedDirs $target 'deps' 3)) {
    Get-ChildItem -LiteralPath $depsDir.FullName -File |
        Where-Object {
            (-not (Test-ReparsePoint $_)) -and
            $_.Name -match $artifact -and (
                $crateNames.Contains($Matches[2]) -or
                ([string]::IsNullOrEmpty($Matches[1]) -and $testNames.Contains($Matches[2]) -and $Matches[4] -in @('.exe', '.pdb', '.d'))
            )
        } |
        Group-Object { $null = $_.Name -match $artifact; '{0}|{1}|{2}' -f $Matches[1], $Matches[2], $Matches[4] } |
        ForEach-Object {
            $_.Group | Sort-Object LastWriteTime -Descending | Select-Object -Skip 1 |
                ForEach-Object { Remove-Cache $_.FullName }
        }
}

Write-Host ('Freed (or would free with -WhatIf): {0:N2} GB' -f ($script:freed / 1GB))
