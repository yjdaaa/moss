# NV18 read-only NSIS archive inspection using a disposable Linux container.
# Never execute the Windows setup or install a Windows tool.
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$bundleDir=(Resolve-Path 'target/x86_64-pc-windows-msvc/release/bundle/nsis').Path
$setup=Join-Path $bundleDir 'MOSS_0.1.0_x64-setup.exe'
if(-not (Test-Path -LiteralPath $setup -PathType Leaf)){throw 'Dedicated NSIS missing'}
if($bundleDir.Contains(',')){throw 'Unsafe Docker mount separator in path'}
& docker image inspect ubuntu:22.04 --format '{{.Id}}' *> $null
if($LASTEXITCODE -ne 0){throw 'No cached trusted Ubuntu base image'}
$id=[Guid]::NewGuid().ToString('N')
$tag='nv18-nsis-inspector:'+ $id.Substring(0,16)
$stage=Join-Path ([IO.Path]::GetTempPath()) ('nv18-nsis-inspector-'+$id)
[IO.Directory]::CreateDirectory($stage)|Out-Null
$built=$false
try{
  $dockerfile=@'
FROM ubuntu:22.04
LABEL nv18.inspector.role="task-owned-readonly"
RUN set -eu; \
    apt-get -o Acquire::AllowInsecureRepositories=false update; \
    DEBIAN_FRONTEND=noninteractive apt-get -o APT::Get::AllowUnauthenticated=false -y --no-install-recommends install p7zip-full; \
    rm -rf /var/lib/apt/lists/*
USER 65534:65534
ENTRYPOINT ["/usr/bin/7z"]
'@
  [IO.File]::WriteAllText((Join-Path $stage 'Dockerfile'),$dockerfile)
  & docker build --pull=false --no-cache --tag $tag $stage
  if($LASTEXITCODE -ne 0){throw 'Signed Ubuntu p7zip build failed'}
  $built=$true
  $args=@('run','--rm','--pull','never','--network','none','--read-only','--cap-drop','ALL',
    '--security-opt','no-new-privileges','--pids-limit','16','--memory','256m',
    '--user','65534:65534','--mount',("type=bind,source="+$bundleDir+",target=/bundle,readonly"),
    $tag,'l','-slt','/bundle/MOSS_0.1.0_x64-setup.exe')
  $listing=@(& docker @args 2>&1)
  $rc=$LASTEXITCODE
  Write-Output ('NV18_NSIS_ARCHIVE_7ZIP_EXIT='+$rc)
  if($rc -ne 0){
    foreach($line in $listing|Select-Object -Last 8){Write-Output ('NV18_INSPECT_DIAGNOSTIC='+[string]$line)}
    throw 'Cannot independently list NSIS; verification incomplete'
  }
  $paths=@($listing|Where-Object {$_ -match '^Path = '})
  Write-Output ('NV18_ARCHIVE_ENTRIES='+$paths.Count)
  $required=@(
    'moss-desktop.exe',
    'moss-voice-local.exe',
    'licenses/voice-local/COPYING',
    'licenses/voice-local/THIRD_PARTY_NOTICES.md',
    'licenses/voice-local/source/Cargo.lock',
    'licenses/voice-local/source/apps/moss-voice-local/src/main.rs',
    'licenses/voice-local/source/crates/moss-voice-local-proto/src/lib.rs'
  )
  foreach($needle in $required){
    $found=@($paths|Where-Object {
      ([string]$_).Replace('Path = ','').Trim() -eq $needle
    })
    Write-Output ('NV18_ARCHIVE_REQUIRED_'+$needle+'='+($found.Count -eq 1))
    if($found.Count -ne 1){throw ('Missing or duplicate mandatory NSIS item: '+$needle)}
  }
  $forbidden=@($paths|Where-Object {
    ([string]$_) -match '([/\\]|^)(\.env(\..*)?|moss\.sqlite|[^/\\]+\.(pfx|pem|key))$' -or
    ([string]$_) -match '[/\\]target[/\\]'
  })
  if($forbidden.Count -gt 0){throw 'NSIS contains private files or compiler target cache'}
  Write-Output 'NV18_NO_PRIVATE_FILES_IN_NSIS_LIST_PASS'

  $setupHash=(Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
  $manifests=@(Get-ChildItem -LiteralPath (Join-Path (Resolve-Path '.').Path 'target') -Directory -Filter 'nv18-*' -ErrorAction SilentlyContinue |
    ForEach-Object {Join-Path $_.FullName 'release-manifest.json'} |
    Where-Object {Test-Path -LiteralPath $_ -PathType Leaf})
  $release=$null
  foreach($path in $manifests){
    $candidate=Get-Content -LiteralPath $path -Raw|ConvertFrom-Json
    if($candidate.files.installer.sha256 -eq $setupHash){$release=$candidate;break}
  }
  if($null -eq $release){throw 'No matching verified NV18 manifest for this NSIS'}
  $check=@'
set -eu
7z x -so /bundle/MOSS_0.1.0_x64-setup.exe moss-voice-local.exe | sha256sum
7z x -so /bundle/MOSS_0.1.0_x64-setup.exe moss-desktop.exe | sha256sum
'@
  $extract=@('run','--rm','--pull','never','--network','none','--read-only','--cap-drop','ALL',
    '--security-opt','no-new-privileges','--pids-limit','16','--memory','256m',
    '--user','65534:65534','--mount',("type=bind,source="+$bundleDir+",target=/bundle,readonly"),
    '--entrypoint','/bin/sh',$tag,'-c',$check)
  $digests=@(& docker @extract 2>&1)
  if($LASTEXITCODE -ne 0){throw 'NSIS in-memory extraction failed'}
  $hashes=@($digests|Where-Object{([string]$_) -match '^[a-f0-9]{64}\s+-$'})
  if($hashes.Count -ne 2){throw 'NSIS extracted binary hashes missing'}
  $helperHash=([string]$hashes[0]).Substring(0,64)
  $desktopHash=([string]$hashes[1]).Substring(0,64)
  Write-Output ('NV18_EXTRACTED_HELPER_SHA256='+$helperHash)
  Write-Output ('NV18_MANIFEST_HELPER_SHA256='+$release.files.helper.sha256)
  Write-Output ('NV18_EXTRACTED_DESKTOP_SHA256='+$desktopHash)
  Write-Output ('NV18_MANIFEST_DESKTOP_SHA256='+$release.files.desktop.sha256)
  if($helperHash -ne $release.files.helper.sha256){
    throw 'NSIS helper hash does not match verified manifest'
  }
  $validDesktop=$desktopHash -eq $release.files.desktop.sha256
  if(-not $validDesktop){
    # Tauri NSIS bundles legitimately patch its internal binary marker:
    # __TAURI_BUNDLE_TYPE_VAR_UNK -> __TAURI_BUNDLE_TYPE_VAR_NSS.
    # Validate EXACTLY those 3 contiguous bytes, with no other changes.
    $sourceExe=(Resolve-Path 'target/x86_64-pc-windows-msvc/release/moss-desktop.exe').Path
    $sourceHash=(Get-FileHash -LiteralPath $sourceExe -Algorithm SHA256).Hash.ToLowerInvariant()
    if($sourceHash -ne $release.files.desktop.sha256){
      throw 'Compiled desktop EXE itself changed after release manifest'
    }
    $diagnostic=@'
set -eu
printf 'EXTRACTED_DESKTOP_BYTES='
7z x -so /bundle/MOSS_0.1.0_x64-setup.exe moss-desktop.exe | wc -c
printf 'BUILD_DESKTOP_BYTES='
wc -c < /compiled-desktop.exe
echo 'DIFFERING_OFFSETS_BEGIN'
7z x -so /bundle/MOSS_0.1.0_x64-setup.exe moss-desktop.exe |
  cmp -l - /compiled-desktop.exe |
  awk 'NR<=6{print} END{print "TOTAL_DIFF_BYTES=" NR}' || true
echo 'DIFFERING_OFFSETS_END'
'@
    $diagArgs=@('run','--rm','--pull','never','--network','none','--read-only','--cap-drop','ALL',
      '--security-opt','no-new-privileges','--pids-limit','16','--memory','256m',
      '--user','65534:65534','--mount',("type=bind,source="+$bundleDir+",target=/bundle,readonly"),
      '--mount',("type=bind,source="+$sourceExe+",target=/compiled-desktop.exe,readonly"),
      '--entrypoint','/bin/sh',$tag,'-c',$diagnostic)
    $details=@(& docker @diagArgs 2>&1)
    if($LASTEXITCODE -ne 0){throw 'Cannot independently compare the embedded desktop EXE'}
    foreach($line in $details){Write-Output ('NV18_PE_PATCH_COMPARE='+[string]$line)}
    $expectedCount=@($details|Where-Object{([string]$_) -eq 'TOTAL_DIFF_BYTES=3'})
    $sizeEmbedded=@($details|Where-Object{([string]$_) -match '^EXTRACTED_DESKTOP_BYTES=\d+$'})
    $sizeSource=@($details|Where-Object{([string]$_) -match '^BUILD_DESKTOP_BYTES=\d+$'})
    if($expectedCount.Count -ne 1 -or $sizeEmbedded.Count -ne 1 -or
      $sizeSource.Count -ne 1 -or ($sizeEmbedded[0] -replace '.*=','') -ne
      ($sizeSource[0] -replace '.*=','')){
      throw 'Unexpected desktop binary difference count or size'
    }
    $diff=@($details|Where-Object{([string]$_) -match '^\s*\d+\s+\d+\s+\d+\s*$'})
    if($diff.Count -ne 3){throw 'Unexpected desktop binary patch extent'}
    $positions=@()
    $octal=@(@('116','125'),@('123','116'),@('123','113'))
    for($i=0;$i -lt 3;$i++){
      $parts=([string]$diff[$i]).Trim() -split '\s+'
      if($parts[1] -ne $octal[$i][0] -or $parts[2] -ne $octal[$i][1]){
        throw 'Tauri marker byte mismatch; not an approved NSIS patch'
      }
      $positions += [long]$parts[0]
      if($i -gt 0 -and $positions[$i] -ne ($positions[$i-1]+1)){
        throw 'Noncontiguous desktop executable patch'
      }
    }
    $prefix=[Text.Encoding]::ASCII.GetBytes('__TAURI_BUNDLE_TYPE_VAR_')
    $markerOffset=[long]$positions[0]-1-$prefix.Length
    if($markerOffset -lt 0){throw 'Desktop bundle patch marker offset invalid'}
    $stream=[IO.File]::OpenRead($sourceExe)
    try{
      [void]$stream.Seek($markerOffset,[IO.SeekOrigin]::Begin)
      $tagBytes=New-Object byte[] ($prefix.Length+3)
      if($stream.Read($tagBytes,0,$tagBytes.Length) -ne $tagBytes.Length){
        throw 'Cannot read compiled bundle type marker'
      }
      if([Text.Encoding]::ASCII.GetString($tagBytes) -ne '__TAURI_BUNDLE_TYPE_VAR_UNK'){
        throw 'Patch is not at the known Tauri bundle-type marker'
      }
    }finally{$stream.Dispose()}
    # Change those three bytes only in memory, then prove full SHA-256
    # equivalence to the embedded executable. Do NOT write or patch the disk.
    $bytes=[IO.File]::ReadAllBytes($sourceExe)
    $bytes[[int]($positions[0]-1)]=[byte]78
    $bytes[[int]($positions[1]-1)]=[byte]83
    $bytes[[int]($positions[2]-1)]=[byte]83
    $patchedHash=[Convert]::ToHexString(
      [Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
    if($patchedHash -ne $desktopHash){
      throw 'Desktop NSIS bytes differ beyond approved Tauri NSS marker'
    }
    Write-Output ('NV18_TAURI_NSS_BUNDLE_PATCH_SHA256='+$desktopHash)
    Write-Output 'NV18_ONLY_APPROVED_TAURI_3_BYTE_MARKER_PATCH_PASS'
  }
  Write-Output ('NV18_ARCHIVE_EMBEDDED_HELPER_SHA256='+$helperHash)
  Write-Output ('NV18_ARCHIVE_EMBEDDED_DESKTOP_SHA256='+$desktopHash)
  Write-Output 'NV18_NSIS_EMBEDDED_EXECUTABLES_VALIDATED'
  Write-Output 'NV18_NSIS_READONLY_ARCHIVE_INSPECTION_PASS'
}finally{
  if($built){& docker image rm $tag *> $null;Write-Output 'NV18_TASK_OWNED_INSPECTOR_IMAGE_REMOVED'}
  $temp=[IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
  $parent=[IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($stage)).TrimEnd('\')
  $ownedName=[IO.Path]::GetFileName($stage).StartsWith('nv18-nsis-inspector-',[StringComparison]::Ordinal)
  if(($parent.Equals($temp,[StringComparison]::OrdinalIgnoreCase)) -and $ownedName -and
      (Test-Path -LiteralPath $stage -PathType Container)){
    Remove-Item -LiteralPath $stage -Recurse -Force
  }
  Write-Output 'NV18_INSPECTOR_TEMP_FILES_CLEANED'
}
