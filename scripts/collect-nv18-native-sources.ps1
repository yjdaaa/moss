# Public, pinned native source/license collection. Never grants release approval.
[CmdletBinding()]
param()
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$repo=(Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$root=Join-Path $repo 'apps/moss-voice-local/native-license-evidence'
$manifest=Join-Path $repo 'apps/moss-voice-local/native-source-evidence.json'
if(Test-Path -LiteralPath $manifest){
  $previous=Get-Content -LiteralPath $manifest -Raw|ConvertFrom-Json
  if($previous.review_status -ne 'pending'){throw 'Refusing to overwrite independently reviewed evidence'}
}
New-Item -ItemType Directory -Path $root -Force | Out-Null
$components=@(
 @{id='sherpa';url='https://github.com/k2-fsa/sherpa-onnx/archive/refs/tags/v1.13.8.tar.gz';hash='b0374cc56dbc186d442ae73d5de743bb092470b640c4c50ce7b029044c0c4fa8';libs=@('sherpa-onnx-c-api','sherpa-onnx-core');licenses=@('LICENSE')},
 @{id='decoder';url='https://github.com/k2-fsa/kaldi-decoder/archive/refs/tags/v0.3.0.tar.gz';hash='b9f34cfb4fd3b1344100eead79ef4d37aa15962274b9e3056de345021f76a1b0';libs=@('kaldi-decoder-core');licenses=@('LICENSE')},
 @{id='kaldifst';url='https://github.com/k2-fsa/kaldifst/archive/refs/tags/v1.8.0.tar.gz';hash='3f247b7e5a2409071202f5e2bc6200060f66728c0a3443c03923ad2723e040b3';libs=@('sherpa-onnx-kaldifst-core');licenses=@('LICENSE')},
 @{id='openfst';url='https://github.com/csukuangfj/openfst/archive/refs/tags/v1.8.5-2026-04-10.tar.gz';hash='c3549940384cbe4fa9f18c2bcfb1bfbd0a80492fd1b0bfa27433cee395a6a199';libs=@('sherpa-onnx-fst','sherpa-onnx-fstfar');licenses=@('COPYING')},
 @{id='fbank';url='https://github.com/csukuangfj/kaldi-native-fbank/archive/refs/tags/v1.22.3.tar.gz';hash='9176cc66fc7ce1edf85cf355b06e320c57db6297df74277f575183468893cf61';libs=@('kaldi-native-fbank-core');licenses=@('LICENSE')},
 @{id='kissfft';url='https://github.com/mborgerding/kissfft/archive/febd4caeed32e33ad8b2e0bb5ea77542c40f18ec.zip';hash='497103e664168ebe39580b757adbe616f6cf85a16572af581ca7bc42d0ab13fd';libs=@('kissfft-float');licenses=@('COPYING')},
 @{id='piper';url='https://github.com/csukuangfj/piper-phonemize/archive/f3ff95afc03640bc1399e113e83361192a2fafb4.zip';hash='d9cca4e2bdc7d6dd8dffb96a4668283dbd3f77a9c194a3e530c1e8eba9406a5d';libs=@('piper_phonemize');licenses=@('LICENSE.md','licenses/uni-algo/LICENSE.md')},
 @{id='espeak';url='https://github.com/csukuangfj/espeak-ng/archive/ed530aa113046142eb5115cf2fc9157854d0ffe1.zip';hash='e4e262cbe34f7fe21f91f1ba3397f2728e1f30eafbae7853f2b753a9ed13f0dd';libs=@('espeak-ng','ucd');licenses=@('COPYING','COPYING.UCD','src/ucd-tools/COPYING','src/ucd-tools/COPYING.UCD')},
 @{id='sentencepiece';url='https://github.com/pkufool/simple-sentencepiece/archive/refs/tags/v0.7.tar.gz';hash='1748a822060a35baa9f6609f84efc8eb54dc0e74b9ece3d82367b7119fdc75af';libs=@('ssentencepiece_core');licenses=@('LICENSE')},
 @{id='ort';url='https://github.com/microsoft/onnxruntime/archive/refs/tags/v1.28.2.tar.gz';hash=$null;libs=@('onnxruntime');licenses=@('LICENSE','ThirdPartyNotices.txt')}
)
$entries=[Collections.Generic.List[object]]::new()
foreach($component in $components){
  $extension=if($component.url.EndsWith('.zip')){'zip'}else{'tar.gz'}
  $archive=Join-Path $root ($component.id+'.'+$extension)
  if(-not(Test-Path -LiteralPath $archive)){
    $partial=$archive+'.'+[Guid]::NewGuid().ToString('N')+'.partial'
    Invoke-WebRequest -Uri $component.url -OutFile $partial -TimeoutSec 300
    Move-Item -LiteralPath $partial -Destination $archive -ErrorAction Stop
  }
  $file=Get-Item -LiteralPath $archive
  if($file.Attributes -band [IO.FileAttributes]::ReparsePoint){throw 'Source archive reparse refused'}
  $hash=(Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
  if($component.hash -and $hash -ne $component.hash){throw ('Source hash mismatch: '+$component.id)}
  $paths=@(& tar -tf $archive)
  if($LASTEXITCODE -ne 0 -or $paths.Count -eq 0){throw 'Source archive listing failed'}
  $prefix=($paths[0] -split '/')[0]
  if($prefix -notmatch '^[A-Za-z0-9._-]+$'){throw 'Archive prefix refused'}
  $licenseFiles=[Collections.Generic.List[object]]::new()
  foreach($license in $component.licenses){
    $entry=$prefix+'/'+$license
    if($entry -cnotin $paths){throw ('Missing source license: '+$component.id+'/'+$license)}
    $relative=$component.id+'-'+($license.Replace('/','_'))+'.txt'
    $destination=Join-Path $root $relative
    if(-not(Test-Path -LiteralPath $destination)){
      $start=[Diagnostics.ProcessStartInfo]::new('tar')
      $start.ArgumentList.Add('-xOf');$start.ArgumentList.Add($archive);$start.ArgumentList.Add($entry)
      $start.RedirectStandardOutput=$true;$start.UseShellExecute=$false;$start.CreateNoWindow=$true
      $process=[Diagnostics.Process]::Start($start)
      $output=[IO.File]::Open($destination,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write)
      try{$process.StandardOutput.BaseStream.CopyTo($output)}finally{$output.Dispose()}
      $process.WaitForExit();if($process.ExitCode -ne 0){throw 'License extraction failed'}
    }
    $licenseFile=Get-Item -LiteralPath $destination
    if($licenseFile.Length -eq 0 -or $licenseFile.Length -gt 1048576 -or ($licenseFile.Attributes -band [IO.FileAttributes]::ReparsePoint)){throw 'License file invalid'}
    $licenseFiles.Add(@{file=$relative;sha256=(Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToLowerInvariant();source_entry=$entry})
  }
  foreach($library in $component.libs){
    $entries.Add(@{name=$library;source_url=$component.url;source_file=$file.Name;source_sha256=$hash;source_bytes=$file.Length;
      hash_basis=$(if($component.hash){'pinned_upstream_or_independently_measured_archive'}else{'download_measured_unreviewed'});
      license_file=$licenseFiles[0].file;license_sha256=$licenseFiles[0].sha256;additional_licenses=@($licenseFiles.ToArray());
      correspondence='upstream_build_candidate_pending_review'})
  }
  Write-Output ('NV18_COLLECTED='+$component.id+' SHA256='+$hash+' BYTES='+$file.Length)
}
@{review_status='pending';reviewed_by='';archive_sha256='56ffcf3c454c1f14f7bc9887286cc8143e7e542dc632804e1c447d5f8d534eaf';
  limitations=@('ORT source tar does not contain all recursive submodules or all pinned cmake/deps.txt archives','Rust/native transitive correspondence and license obligations require review; registry collection alone does not prove closure','Binary/source correspondence requires independent review');
  libraries=@($entries.ToArray())} | ConvertTo-Json -Depth 9 | Set-Content -LiteralPath $manifest -Encoding utf8
Write-Output 'NV18_COLLECTION_COMPLETE_REVIEW_PENDING'
