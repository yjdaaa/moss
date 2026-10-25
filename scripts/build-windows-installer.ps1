# Build both Windows executables from this checkout. Never install or operate hardware.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# NV18 safety shim: preserve historical implementation below for audit, but
# never enter its old recursive cleanup/overwrite path. Delegate to the
# non-destructive unique-staging builder and propagate any thrown failure.
$safeBuilder = Join-Path $PSScriptRoot 'build-nv18-sidecar-safe.ps1'
if (-not (Test-Path -LiteralPath $safeBuilder -PathType Leaf)) {
  throw 'Missing non-destructive NV18 installer builder; refusing legacy cleanup.'
}
& $safeBuilder
return

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$triple = 'x86_64-pc-windows-msvc'
if ($env:OS -ne 'Windows_NT') { throw 'Windows installer requires Windows.' }
$desktop = Join-Path $repo 'apps/desktop/src-tauri'
$helperRoot = Join-Path $repo 'apps/moss-voice-local'
$helperManifest = Join-Path $helperRoot 'Cargo.toml'
$helper = Join-Path $repo "target/$triple/release/moss-voice-local.exe"
$binaryDir = Join-Path $desktop 'binaries'
$sidecar = Join-Path $binaryDir "moss-voice-local-$triple.exe"
$resourceDir = Join-Path $desktop 'bundle-resources/voice-local'
$sourceDir = Join-Path $resourceDir 'source'
$manifestPath = Join-Path $repo 'target/release-manifest.json'
function Must-Exist([string]$path) {
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing required release input: $path" }
}
function Copy-Required([string]$from, [string]$to) {
  Must-Exist $from
  $parent = Split-Path -Parent $to
  New-Item -ItemType Directory -Path $parent -Force | Out-Null
  Copy-Item -LiteralPath $from -Destination $to -Force
}
# No stale sidecar may be included if the fresh helper build fails.
if (Test-Path -LiteralPath $sidecar) { Remove-Item -LiteralPath $sidecar -Force }
& cargo build --release --manifest-path $helperManifest --target $triple --target-dir (Join-Path $repo 'target')
if ($LASTEXITCODE -ne 0) { throw 'Voice helper release build failed.' }
Must-Exist $helper
if ((Get-Item -LiteralPath $helper).Length -le 0) { throw 'Voice helper is empty.' }
New-Item -ItemType Directory -Path $binaryDir -Force | Out-Null
Copy-Required $helper $sidecar
if (Test-Path -LiteralPath $resourceDir) { Remove-Item -LiteralPath $resourceDir -Recurse -Force }
# GPL helper corresponding source, MIT IPC dependency, and lockfile, without target or local settings.
$helperSources = @('Cargo.toml','Cargo.lock','build.rs','COPYING','README.md')
foreach ($item in $helperSources) { Copy-Required (Join-Path $helperRoot $item) (Join-Path $sourceDir "apps/moss-voice-local/$item") }
New-Item -ItemType Directory -Path (Join-Path $sourceDir 'apps/moss-voice-local') -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $helperRoot 'src') -Destination (Join-Path $sourceDir 'apps/moss-voice-local/src') -Recurse -Force
Copy-Required (Join-Path $repo 'crates/moss-voice-local-proto/Cargo.toml') (Join-Path $sourceDir 'crates/moss-voice-local-proto/Cargo.toml')
New-Item -ItemType Directory -Path (Join-Path $sourceDir 'crates/moss-voice-local-proto') -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $repo 'crates/moss-voice-local-proto/src') -Destination (Join-Path $sourceDir 'crates/moss-voice-local-proto/src') -Recurse -Force
Copy-Required (Join-Path $helperRoot 'COPYING') (Join-Path $resourceDir 'COPYING')
Copy-Required (Join-Path $helperRoot 'README.md') (Join-Path $resourceDir 'README.md')
Copy-Required (Join-Path $helperRoot 'THIRD_PARTY_NOTICES.md') (Join-Path $resourceDir 'THIRD_PARTY_NOTICES.md')
# Cargo path dependencies expect this relative source tree; root lockfile pins the dependency graph.
Copy-Required (Join-Path $repo 'Cargo.lock') (Join-Path $sourceDir 'Cargo.lock')
Push-Location (Join-Path $repo 'frontend')
try {
  & npm run build
  if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed.' }
} finally { Pop-Location }
$bundleStartedAt = (Get-Date).ToUniversalTime()
Push-Location $desktop
try {
  & cargo tauri build --target $triple --config tauri.voice-sidecar.conf.json
  if ($LASTEXITCODE -ne 0) { throw 'Tauri NSIS bundle build failed.' }
} finally { Pop-Location }
# Hash this run's inputs; installation and clean-VM acceptance are tracked separately.
$nsis = @(Get-ChildItem -LiteralPath (Join-Path $repo "target/$triple/release/bundle/nsis") -Filter '*.exe' -File -ErrorAction Stop | Sort-Object LastWriteTime -Descending)
if ($nsis.Count -eq 0) { throw 'No NSIS installer produced.' }
if ($nsis[0].LastWriteTimeUtc -lt $bundleStartedAt.AddSeconds(-5)) { throw 'Installer timestamp is older than this build; refusing stale bundle.' }
$exe = Join-Path $repo "target/$triple/release/moss-desktop.exe"
Must-Exist $exe
$entries = @{}
foreach ($entry in @(@{key='desktop';path=$exe},@{key='helper';path=$sidecar},@{key='installer';path=$nsis[0].FullName})) {
  $file = Get-Item -LiteralPath $entry.path
  $entries[$entry.key] = @{ name=$file.Name; bytes=$file.Length; sha256=(Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
}
$protocolSource = Get-Content -LiteralPath (Join-Path $repo 'crates/moss-voice-local-proto/src/lib.rs') -Raw
$schemaSource = Get-Content -LiteralPath (Join-Path $repo 'crates/laser-runtime/src/db.rs') -Raw
$protocolMatch = [regex]::Match($protocolSource, 'pub const PROTOCOL_VERSION:\s*u32\s*=\s*(\d+)')
$schemaMatch = [regex]::Match($schemaSource, 'pub const SCHEMA_VERSION:\s*i64\s*=\s*(\d+)')
if (-not $protocolMatch.Success -or -not $schemaMatch.Success) { throw 'Cannot determine release protocol/schema versions from source.' }
$sourceLines = @(Get-ChildItem -LiteralPath $sourceDir -Recurse -File | Sort-Object FullName | ForEach-Object {
  $relative = $_.FullName.Substring($sourceDir.Length).TrimStart([char[]]@('\','/')).Replace('\','/')
  "$relative=$((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant())"
})
$sourceHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes(($sourceLines -join "`n")))).ToLowerInvariant()
$manifest = @{ product_version=(Get-Content -LiteralPath (Join-Path $desktop 'tauri.conf.json') -Raw | ConvertFrom-Json).version; helper_protocol=[int]$protocolMatch.Groups[1].Value; db_schema=[int]$schemaMatch.Groups[1].Value; source_tree_sha256=$sourceHash; generated_at=(Get-Date).ToUniversalTime().ToString('o'); files=$entries; validation='built_not_installed' }
$manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestPath -Encoding utf8
Write-Host "NSIS installer: $($nsis[0].FullName)"
Write-Host "Release manifest: $manifestPath"
Write-Host 'Helper bundled with corresponding local source, COPYING and notices. Installer execution/clean VM not verified.'
