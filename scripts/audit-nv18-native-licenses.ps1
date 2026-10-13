# NV18 native dependency/license audit - read-only, offline, no install.
# Only scans the pinned sherpa-onnx release cache and project notices.
# -RequireComplete fails closed for release gating; ordinary diagnostics always
# reports completeness without modifying build artifacts or user data.
[CmdletBinding()]
param([switch]$RequireComplete)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$release = 'sherpa-onnx-v1.13.8-win-x64-static-MT-Release-lib'
$cache = Join-Path $repo 'target/sherpa-onnx-prebuilt'
$archive = Join-Path $cache ($release + '.tar.bz2')
$extracted = Join-Path $cache $release
$notices = Join-Path $repo 'apps/moss-voice-local/THIRD_PARTY_NOTICES.md'
$expectedArchiveSha256 = '56ffcf3c454c1f14f7bc9887286cc8143e7e542dc632804e1c447d5f8d534eaf'
$expectedArchiveBytes = 123206268
# Names are taken from the pinned sherpa-onnx-sys 1.13.8 build.rs link list.
$requiredNativeLibs = @(
    'sherpa-onnx-c-api', 'sherpa-onnx-core', 'kaldi-decoder-core',
    'sherpa-onnx-kaldifst-core', 'sherpa-onnx-fstfar', 'sherpa-onnx-fst',
    'kaldi-native-fbank-core', 'kissfft-float', 'piper_phonemize',
    'espeak-ng', 'ucd', 'onnxruntime', 'ssentencepiece_core'
)
$errors = [System.Collections.Generic.List[string]]::new()

if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) {
    $errors.Add('pinned_archive_missing')
} else {
    $item = Get-Item -LiteralPath $archive -ErrorAction Stop
    $actualHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $expectedArchiveSha256 -or $item.Length -ne $expectedArchiveBytes) {
        $errors.Add('pinned_archive_sha256_or_size_mismatch')
    }
    Write-Output ('NV18_NATIVE_ARCHIVE_SHA256=' + $actualHash)
    Write-Output ('NV18_NATIVE_ARCHIVE_BYTES=' + $item.Length)
}

if (-not (Test-Path -LiteralPath $extracted -PathType Container)) {
    $errors.Add('pinned_archive_extraction_missing')
} else {
    $libs = @(Get-ChildItem -LiteralPath $extracted -Recurse -File -ErrorAction Stop |
        Where-Object { $_.Extension -eq '.lib' })
    $names = @($libs | ForEach-Object { $_.BaseName })
    Write-Output ('NV18_NATIVE_STATIC_LIB_COUNT=' + $libs.Count)
    foreach ($required in $requiredNativeLibs) {
        if ($required -cnotin $names) {
            $errors.Add('required_native_library_missing:' + $required)
        }
    }
    $licenseTexts = @(Get-ChildItem -LiteralPath $extracted -Recurse -File -ErrorAction Stop |
        Where-Object { $_.Name -match '^(?i:LICENSE|COPYING|NOTICE|THIRD_PARTY)' })
    Write-Output ('NV18_NATIVE_ARCHIVE_LICENSE_TEXT_FILES=' + $licenseTexts.Count)
    if ($licenseTexts.Count -eq 0) {
        Write-Output 'NV18_ARCHIVE_HAS_NO_EMBEDDED_LICENSES_SIDECAR_EVIDENCE_REQUIRED=True'
    }
    # The binary archive alone does not prove corresponding native source.
    # Independently reviewed source provenance is required per static library.
    $evidence = Join-Path $repo 'apps/moss-voice-local/native-source-evidence.json'
    Write-Output ('NV18_REVIEWED_NATIVE_SOURCE_EVIDENCE_PRESENT=' +
        (Test-Path -LiteralPath $evidence -PathType Leaf))
    if (-not (Test-Path -LiteralPath $evidence -PathType Leaf)) {
        $errors.Add('corresponding_native_source_provenance_unreviewed')
    } else {
        # A human-entered hash is not proof: every claimed source and license
        # must be a real, task-owned file with a matching SHA-256.
        $record = Get-Content -LiteralPath $evidence -Raw -ErrorAction Stop | ConvertFrom-Json
        $sourcesRoot = Join-Path $repo 'apps/moss-voice-local/native-license-evidence'
        if ($record.review_status -ne 'approved' -or
            [string]::IsNullOrWhiteSpace([string]$record.reviewed_by)) {
            $errors.Add('native_source_correspondence_review_pending')
        }
        if ($record.PSObject.Properties['limitations'] -and @($record.limitations).Count -gt 0){
            $errors.Add('transitive_source_license_closure_incomplete')
        }
        if (
            $record.archive_sha256 -ne $expectedArchiveSha256 -or
            @($record.libraries).Count -ne $requiredNativeLibs.Count -or
            -not (Test-Path -LiteralPath $sourcesRoot -PathType Container)) {
            $errors.Add('native_source_evidence_incomplete')
        } else {
            $safeRoot = [IO.Path]::GetFullPath($sourcesRoot).TrimEnd('\', '/') +
                [IO.Path]::DirectorySeparatorChar
            foreach ($required in $requiredNativeLibs) {
                $entry = @($record.libraries | Where-Object { $_.name -ceq $required })
                if ($entry.Count -ne 1) {
                    $errors.Add('native_source_evidence_missing_or_duplicate:' + $required)
                    continue
                }
                $checks=@(@{file=$entry[0].source_file;hash=$entry[0].source_sha256;role='source'},@{file=$entry[0].license_file;hash=$entry[0].license_sha256;role='license'})
                if($entry[0].PSObject.Properties['additional_licenses']){
                    foreach($extra in $entry[0].additional_licenses){$checks+=@{file=$extra.file;hash=$extra.sha256;role='additional_license'}}
                }
                foreach ($check in $checks) {
                    $role=$check.role
                    $relative = [string]$check.file
                    $expectedHash = [string]$check.hash
                    if ($relative -notmatch '^[a-zA-Z0-9][a-zA-Z0-9._/-]{0,180}$' -or
                        $relative -match '(^|/)\.\.?(/|$)' -or
                        $expectedHash -notmatch '^[a-f0-9]{64}$') {
                        $errors.Add('native_source_evidence_invalid_path_or_hash:' + $required + ':' + $role)
                        continue
                    }
                    $full = [IO.Path]::GetFullPath((Join-Path $sourcesRoot $relative))
                    if (-not $full.StartsWith($safeRoot, [StringComparison]::OrdinalIgnoreCase) -or
                        -not (Test-Path -LiteralPath $full -PathType Leaf)) {
                        $errors.Add('native_source_evidence_file_missing:' + $required + ':' + $role)
                        continue
                    }
                    $file = Get-Item -LiteralPath $full -Force -ErrorAction Stop
                    if (($file.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
                        (Get-FileHash -LiteralPath $full -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expectedHash) {
                        $errors.Add('native_source_evidence_hash_or_reparse_mismatch:' + $required + ':' + $role)
                    }
                }
            }
        }
    }

}

if (-not (Test-Path -LiteralPath $notices -PathType Leaf)) {
    $errors.Add('third_party_notices_missing')
} else {
    $noticeText = Get-Content -LiteralPath $notices -Raw
    foreach ($required in $requiredNativeLibs) {
        if ($noticeText.IndexOf($required, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
            $errors.Add('native_component_missing_from_notices:' + $required)
        }
    }
}

# Registry evidence is part of the sidecar source offer too. Recheck real
# bytes against Cargo.lock; a JSON receipt alone cannot prove collection.
$rustEvidence=Join-Path $repo 'apps/moss-voice-local/rust-source-evidence.json'
$rustRoot=Join-Path $repo 'apps/moss-voice-local/rust-license-evidence'
$rustLock=Join-Path $repo 'apps/moss-voice-local/Cargo.lock'
if(-not(Test-Path -LiteralPath $rustEvidence -PathType Leaf)){
    $errors.Add('rust_source_evidence_missing')
}else{
    $rust=Get-Content -LiteralPath $rustEvidence -Raw|ConvertFrom-Json
    $locked=@{}
    foreach($block in [regex]::Split((Get-Content -LiteralPath $rustLock -Raw),'\[\[package\]\]')){
        $pkgName=[regex]::Match($block,'(?m)^name = "([^"]+)"').Groups[1].Value
        $pkgVersion=[regex]::Match($block,'(?m)^version = "([^"]+)"').Groups[1].Value
        $pkgHash=[regex]::Match($block,'(?m)^checksum = "([a-f0-9]{64})"').Groups[1].Value
        if($pkgHash){$locked["$pkgName@$pkgVersion"]=$pkgHash}
    }
    if($rust.cargo_lock_sha256 -ne (Get-FileHash -LiteralPath $rustLock -Algorithm SHA256).Hash.ToLowerInvariant() -or
       @($rust.packages).Count -ne $locked.Count -or $rust.registry_packages -ne $locked.Count){
        $errors.Add('rust_lock_manifest_mismatch')
    }
    if(@($rust.missing_license_texts).Count -gt 0){$errors.Add('rust_license_text_missing')}
    $seen=[Collections.Generic.HashSet[string]]::new()
    foreach($pkg in $rust.packages){
        $key="$($pkg.name)@$($pkg.version)"
        if(-not $seen.Add($key) -or -not $locked.ContainsKey($key) -or $locked[$key] -ne $pkg.source_sha256){
            $errors.Add('rust_package_lock_mismatch:'+$key);continue
        }
        if(@($pkg.license_files).Count -eq 0){$errors.Add('rust_package_license_missing:'+$key)}
        $checks=@(@{file=$pkg.source_file;hash=$pkg.source_sha256})
        foreach($license in $pkg.license_files){$checks+=@{file=$license.file;hash=$license.sha256}}
        foreach($check in $checks){
            $name=[string]$check.file
            if($name -notmatch '^[a-zA-Z0-9][a-zA-Z0-9._+-]{0,180}$' -or $check.hash -notmatch '^[a-f0-9]{64}$'){
                $errors.Add('rust_evidence_invalid:'+$key);continue
            }
            $file=Join-Path $rustRoot $name
            if(-not(Test-Path -LiteralPath $file -PathType Leaf)){$errors.Add('rust_evidence_missing:'+$key);continue}
            $item=Get-Item -LiteralPath $file -Force
            if(($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
               (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant() -ne $check.hash){
                $errors.Add('rust_evidence_hash_or_reparse_mismatch:'+$key)
            }
        }
    }
    Write-Output ('NV18_RUST_REGISTRY_ARCHIVES='+@($rust.packages).Count)
    Write-Output ('NV18_RUST_LICENSE_TEXTS='+(($rust.packages|ForEach-Object{@($_.license_files).Count}|Measure-Object -Sum).Sum))
}

$ortEvidence=Join-Path $repo 'apps/moss-voice-local/native-license-evidence/ort-dependency-evidence.json'
if(Test-Path -LiteralPath $ortEvidence -PathType Leaf){
    $ort=Get-Content -LiteralPath $ortEvidence -Raw|ConvertFrom-Json
    if($ort.release_ready -ne $false -or $ort.review_status -ne 'pending'){$errors.Add('ort_candidate_receipt_cannot_self_approve')}
    if($ort.root_archive_sha256 -ne 'a5003dfd85d66a4d45bd2757c7f813f9e835142e9a936ac04877c788d6afe938'){$errors.Add('ort_root_archive_mismatch')}
    if(@($ort.uncollected_sources).Count -gt 0 -or $ort.gitlink_resolution_error){$errors.Add('ort_candidate_collection_incomplete')}
    $ortRoot=Join-Path $repo 'apps/moss-voice-local/native-license-evidence'
    $collected=@($ort.dependencies|Where-Object collection -eq collected_candidate)
    if(@($ort.dependencies).Count -gt 100){throw 'ORT candidate receipt exceeds bounded entry count'}
    foreach($dep in $collected){
        $checks=@(@{file=$dep.source_file;hash=$dep.source_sha256;sha1=$(if($dep.PSObject.Properties['upstream_sha1']){$dep.upstream_sha1}else{$null})})
        foreach($license in $dep.license_files){$checks+=@{file=$license.file;hash=$license.sha256;sha1=$null}}
        foreach($check in $checks){
            if($check.file -notmatch '^ort-dependencies/[a-zA-Z0-9][a-zA-Z0-9._-]{0,180}$' -or $check.hash -notmatch '^[a-f0-9]{64}$'){$errors.Add('ort_evidence_invalid_path_or_hash');continue}
            $file=Join-Path $ortRoot $check.file
            if(-not(Test-Path -LiteralPath $file -PathType Leaf)){$errors.Add('ort_evidence_file_missing');continue}
            if(((Get-Item -LiteralPath $file -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
                (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant() -ne $check.hash){$errors.Add('ort_evidence_bytes_changed')}
            if($check.sha1 -and (Get-FileHash -LiteralPath $file -Algorithm SHA1).Hash.ToLowerInvariant() -ne $check.sha1){$errors.Add('ort_upstream_sha1_changed')}
        }
    }
    Write-Output ('NV18_ORT_CANDIDATE_ARCHIVES='+$collected.Count)
}

foreach ($issue in $errors) { Write-Output ('NV18_LICENSE_BLOCKER=' + $issue) }
$ready = $errors.Count -eq 0
Write-Output ('NV18_NATIVE_LICENSE_RELEASE_READY=' + $ready)
Write-Output ('NV18_NATIVE_LICENSE_BLOCKER_COUNT=' + $errors.Count)
if ($RequireComplete -and -not $ready) {
    throw 'Native-license release gate FAIL-CLOSED: cannot claim distributable installer.'
}
