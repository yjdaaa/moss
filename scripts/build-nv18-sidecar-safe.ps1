# NV18 Windows voice-sidecar build with unique, non-destructive staging.
# No installation, hardware access, real model inference, or source-tree deletion.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if(-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([Runtime.InteropServices.OSPlatform]::Windows)){throw 'Windows host required'}
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$desktop = Join-Path $repo 'apps/desktop/src-tauri'
$helperRoot = Join-Path $repo 'apps/moss-voice-local'
$target = Join-Path $repo 'target'
$triple = 'x86_64-pc-windows-msvc'
$id = [Guid]::NewGuid().ToString('N')
$relResource = "bundle-resources/nv18-$id/voice-local"
$resource = Join-Path $desktop $relResource
$src = Join-Path $resource 'source'
$binRel = "binaries/nv18-$id/moss-voice-local"
$bin = Join-Path $desktop "binaries/nv18-$id/moss-voice-local-$triple.exe"
$runDir = Join-Path $target "nv18-$id"
$overlay = Join-Path $runDir 'overlay.json'
$manifest = Join-Path $runDir 'release-manifest.json'
$helper = Join-Path $target "$triple/release/moss-voice-local.exe"
function Require-File([string]$path) {
  if(-not(Test-Path -LiteralPath $path -PathType Leaf)){throw "Missing required file: $path"}
}
function Copy-New([string]$from,[string]$to){
  Require-File $from
  New-Item -ItemType Directory -Path (Split-Path -Parent $to) -Force | Out-Null
  if(Test-Path -LiteralPath $to){throw "Refusing to replace staged file: $to"}
  Copy-Item -LiteralPath $from -Destination $to -ErrorAction Stop
}
foreach($directory in @($resource,$runDir,(Split-Path -Parent $bin))) {
  if(Test-Path -LiteralPath $directory){throw "Build staging collision: $directory"}
}
New-Item -ItemType Directory -Path $resource,$runDir,(Split-Path -Parent $bin) | Out-Null
Write-Output 'NV18_BUILDING_HELPER'
& cargo build --release --manifest-path (Join-Path $helperRoot 'Cargo.toml') --target $triple --target-dir $target
if($LASTEXITCODE -ne 0){throw 'Helper compilation failed'}
Require-File $helper
if((Get-Item $helper).Length -le 0){throw 'Empty helper EXE'}
$version = @(& $helper --version 2>&1)
if($LASTEXITCODE -ne 0){throw 'Helper version probe failed'}
Write-Output ('NV18_HELPER_VERSION='+($version -join ' '))
Copy-New $helper $bin

# Only NEW unique resource directories are created. Old untracked source,
# licenses, executable and installer inputs are NOT removed or overwritten.
foreach($file in @('Cargo.toml','Cargo.lock','build.rs','COPYING','README.md')){
  Copy-New (Join-Path $helperRoot $file) (Join-Path $src "apps/moss-voice-local/$file")
}
Copy-Item -LiteralPath (Join-Path $helperRoot 'src') -Destination (Join-Path $src 'apps/moss-voice-local/src') -Recurse -ErrorAction Stop
Copy-New (Join-Path $repo 'crates/moss-voice-local-proto/Cargo.toml') (Join-Path $src 'crates/moss-voice-local-proto/Cargo.toml')
Copy-Item -LiteralPath (Join-Path $repo 'crates/moss-voice-local-proto/src') -Destination (Join-Path $src 'crates/moss-voice-local-proto/src') -Recurse -ErrorAction Stop
Copy-New (Join-Path $repo 'Cargo.lock') (Join-Path $src 'Cargo.lock')
foreach($file in @('COPYING','README.md','THIRD_PARTY_NOTICES.md')){
  Copy-New (Join-Path $helperRoot $file) (Join-Path $resource $file)
}
if(Test-Path -LiteralPath (Join-Path $helperRoot 'native-source-evidence.json')){
  Copy-New (Join-Path $helperRoot 'native-source-evidence.json') (Join-Path $resource 'native-source-evidence.json')
  Copy-Item -LiteralPath (Join-Path $helperRoot 'native-license-evidence') -Destination (Join-Path $resource 'native-license-evidence') -Recurse -ErrorAction Stop
}
if(Test-Path -LiteralPath (Join-Path $helperRoot 'rust-source-evidence.json')){
  Copy-New (Join-Path $helperRoot 'rust-source-evidence.json') (Join-Path $resource 'rust-source-evidence.json')
  Copy-Item -LiteralPath (Join-Path $helperRoot 'rust-license-evidence') -Destination (Join-Path $resource 'rust-license-evidence') -Recurse -ErrorAction Stop
}
$configuration=@{bundle=@{externalBin=@($binRel);resources=@{"$relResource/"='licenses/voice-local/'}}}
$configuration|ConvertTo-Json -Depth 7|Set-Content -LiteralPath $overlay -Encoding utf8

Push-Location (Join-Path $repo 'frontend')
try{& npm run build;if($LASTEXITCODE -ne 0){throw 'Frontend release build failed'}}finally{Pop-Location}
$started=(Get-Date).ToUniversalTime()
Push-Location $desktop
try{& cargo tauri build --target $triple --config $overlay;if($LASTEXITCODE -ne 0){throw 'Sidecar NSIS build failed'}}finally{Pop-Location}
$desktopExe=Join-Path $target "$triple/release/moss-desktop.exe"
Require-File $desktopExe
$nsis=@(Get-ChildItem -LiteralPath (Join-Path $target "$triple/release/bundle/nsis") -Filter '*.exe' -File |
  Sort-Object LastWriteTimeUtc -Descending)
if($nsis.Count -eq 0 -or $nsis[0].LastWriteTimeUtc -lt $started.AddSeconds(-5)){
  throw 'No fresh dedicated NSIS found'
}
$files=@{}
foreach($item in @(@{key='desktop';path=$desktopExe},@{key='helper';path=$bin},@{key='installer';path=$nsis[0].FullName})){
  $f=Get-Item -LiteralPath $item.path
  $files[$item.key]=@{name=$f.Name;bytes=$f.Length;sha256=(Get-FileHash $f.FullName -Algorithm SHA256).Hash.ToLowerInvariant()}
}
$protocolSource=Get-Content -LiteralPath (Join-Path $repo 'crates/moss-voice-local-proto/src/lib.rs') -Raw
$dbSource=Get-Content -LiteralPath (Join-Path $repo 'crates/laser-runtime/src/db.rs') -Raw
$proto=[regex]::Match($protocolSource,'pub const PROTOCOL_VERSION:\s*u32\s*=\s*(\d+)')
$schema=[regex]::Match($dbSource,'pub const SCHEMA_VERSION:\s*i64\s*=\s*(\d+)')
if(-not $proto.Success -or -not $schema.Success){throw 'Cannot verify protocol or schema'}
$hashLines=@(Get-ChildItem -LiteralPath $src -Recurse -File | Sort-Object FullName | ForEach-Object {
  $rel=$_.FullName.Substring($src.Length).TrimStart([char[]]@('\','/')).Replace('\','/')
  $rel+'='+((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant())
})
$joined=[string]::Join([char]10,$hashLines)
$sourceHash=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($joined))).ToLowerInvariant()
$release=@{
 product_version=(Get-Content -LiteralPath (Join-Path $desktop 'tauri.conf.json') -Raw|ConvertFrom-Json).version
 helper_protocol=[int]$proto.Groups[1].Value
 db_schema=[int]$schema.Groups[1].Value
 source_tree_sha256=$sourceHash
 generated_at=(Get-Date).ToUniversalTime().ToString('o')
 files=$files
 validation='built_not_installed_or_license_audited'
}
$release|ConvertTo-Json -Depth 8|Set-Content -LiteralPath $manifest -Encoding utf8
Write-Output ('NV18_BUNDLE='+$nsis[0].FullName)
Write-Output ('NV18_MANIFEST='+$manifest)
Write-Output ('NV18_NSIS_SHA256='+$files.installer.sha256)
Write-Output ('NV18_SOURCE_TREE_SHA256='+$sourceHash)
Write-Output 'NV18_NON_DESTRUCTIVE_BUILD_COMPLETE_NOT_INSTALLED'
