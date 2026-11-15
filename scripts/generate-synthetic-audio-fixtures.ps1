# NV14 deterministic synthetic music codec fixtures only.
# Uses a cached Ubuntu Docker base + GPG-verified Ubuntu ffmpeg package.
# No user music, personal directories, GUI, models or real devices are accessed.
# The temporary helper Docker image is deleted after generating fixtures.
[CmdletBinding()]
param()
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$expectedBase = 'sha256:5ec03bb3441e8b0bf3b4f9cd4629a1ae763010dc3035bb8da3ae6cf026486401'
$actualBase = @(& docker image inspect 'ubuntu:22.04' --format '{{.Id}}' 2>&1)
if ($LASTEXITCODE -ne 0 -or ($actualBase -join '') -ne $expectedBase) {
    throw 'Pinned cached Ubuntu 22.04 base image is missing or changed'
}
$id = [Guid]::NewGuid().ToString('N')
$name = 'nv14-ffmpeg-fixture-' + $id.Substring(0, 16)
$tag = 'moss-nv14-synthetic-ffmpeg:' + $id.Substring(0, 12)
$root = Join-Path ([IO.Path]::GetTempPath()) ('nv14-synthetic-' + $id)
$out = Join-Path $root 'out'
$dest = Join-Path (Get-Location).Path 'crates/moss-voice-server/tests/fixtures'
$filenames = @(
    'sine-48k-stereo.mp3',
    'sine-48k-stereo.flac',
    'sine-48k-stereo.ogg',
    'sine-48k-stereo-opus.ogg'
)
[IO.Directory]::CreateDirectory($out) | Out-Null
try {
    $dockerfile = @'
FROM ubuntu:22.04
RUN set -eu; \
 apt-get -o Acquire::AllowInsecureRepositories=false -o Acquire::AllowDowngradeToInsecureRepositories=false update; \
 DEBIAN_FRONTEND=noninteractive apt-get -o APT::Get::AllowUnauthenticated=false -y --no-install-recommends install ffmpeg; \
 dpkg-query -W ffmpeg; \
 rm -rf /var/lib/apt/lists/*
USER 65534:65534
ENTRYPOINT ["/usr/bin/ffmpeg"]
'@
    [IO.File]::WriteAllText((Join-Path $root 'Dockerfile'), $dockerfile)
    Write-Output 'NV14_BUILD_TEMPORARY_SIGNED_UBUNTU_FFMPEG'
    & docker build --pull=false --network=default --tag $tag $root
    if ($LASTEXITCODE -ne 0) { throw 'Cannot build synthetic-only FFmpeg fixture image' }

    $mountSource = [IO.Path]::GetFullPath($out)
    if ($mountSource -match '[,\r\n]' -or ![IO.Path]::IsPathRooted($mountSource)) {
        throw 'Unsafe synthetic fixture mount path'
    }
    $fixed = @(
        'run', '--rm', '--pull', 'never', '--name', $name,
        '--network', 'none', '--read-only',
        '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
        '--pids-limit', '8', '--memory', '256m', '--cpus', '1',
        '--user', '65534:65534',
        '--mount', ('type=bind,source=' + $mountSource + ',target=/out'),
        '--entrypoint', '/usr/bin/ffmpeg', $tag,
        '-nostdin', '-hide_banner', '-loglevel', 'error',
        '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000:duration=1',
        '-ac', '2'
    )
    $variants = @(
        @('-c:a', 'libmp3lame', '-b:a', '96k', '-y', '/out/sine-48k-stereo.mp3'),
        @('-c:a', 'flac', '-sample_fmt', 's16', '-y', '/out/sine-48k-stereo.flac'),
        @('-c:a', 'libvorbis', '-q:a', '3', '-y', '/out/sine-48k-stereo.ogg'),
        @('-c:a', 'libopus', '-b:a', '64k', '-y', '/out/sine-48k-stereo-opus.ogg')
    )
    foreach ($variant in $variants) {
        & docker @($fixed + $variant)
        if ($LASTEXITCODE -ne 0) { throw ('Synthetic encoding failed: ' + $variant[-1]) }
    }
    foreach ($filename in $filenames) {
        $source = Join-Path $out $filename
        $item = Get-Item -LiteralPath $source -ErrorAction Stop
        if ($item.Length -lt 256 -or $item.Length -gt 131072) {
            throw ('Synthetic media fixture size invalid: ' + $filename)
        }
        $buffer = [IO.File]::ReadAllBytes($source)
        if ($filename.EndsWith('.flac') -and [Text.Encoding]::ASCII.GetString($buffer,0,4) -ne 'fLaC') {
            throw 'FLAC magic mismatch'
        }
        if ($filename.EndsWith('.ogg') -and [Text.Encoding]::ASCII.GetString($buffer,0,4) -ne 'OggS') {
            throw 'Ogg magic mismatch'
        }
        Write-Output ('NV14_SYNTHETIC_FIXTURE='+$filename+' size='+$item.Length+
          ' SHA256='+(Get-FileHash $source -Algorithm SHA256).Hash.ToLowerInvariant())
    }
    [IO.Directory]::CreateDirectory($dest) | Out-Null
    foreach ($filename in $filenames) {
        $destination = Join-Path $dest $filename
        if (Test-Path -LiteralPath $destination) {
            $existing = (Get-FileHash $destination -Algorithm SHA256).Hash
            $incoming = (Get-FileHash (Join-Path $out $filename) -Algorithm SHA256).Hash
            if ($existing -ne $incoming) { throw ('Existing fixture would be overwritten: ' + $filename) }
        } else {
            [IO.File]::Copy((Join-Path $out $filename), $destination)
        }
    }
    Write-Output 'NV14_4_SYNTHETIC_AUDIO_FIXTURES_PRESERVED_IN_PROJECT'
}
finally {
    $owned = @(& docker ps -a --filter ('name=^/' + $name + '$') --format '{{.Names}}' 2>$null)
    if ($owned -contains $name) { & docker rm -f $name *> $null }
    & docker image rm $tag *> $null
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
    $parent = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($root)).TrimEnd('\')
    if ($parent.Equals($temp, [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($root).StartsWith('nv14-synthetic-', [StringComparison]::Ordinal) -and
        (Test-Path -LiteralPath $root -PathType Container)) {
        Remove-Item -LiteralPath $root -Force -Recurse
    }
    Write-Output 'NV14_ONLY_OWN_TEMP_DOCKER_AND_FILES_CLEANED'
}
