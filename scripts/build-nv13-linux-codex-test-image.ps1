# NV13 pinned official Codex binary: checksum verified, offline Docker build.
# No login, no model calls, no Windows system changes, no host credential mounts.
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$releaseTag = 'rust-v0.162.1'
$assetName = 'codex-x86_64-unknown-linux-musl.tar.gz'
$assetSize = 111350218
$sha256 = '86f268d81b898f3e144c5ecff2ad2fda2c5802e042d6195b72538fe0fa125057'
$assetUrl = 'https://github.com/openai/codex/releases/download/' + $releaseTag + '/' + $assetName
$image = 'moss-nv13-codex-test:0.162.1'

$headers = @{
    'Accept' = 'application/vnd.github+json'
    'User-Agent' = 'NV13-Codex-Pinned-Checksum-Offline-Test'
}
$release = Invoke-RestMethod -Uri ('https://api.github.com/repos/openai/codex/releases/tags/' + $releaseTag) -Headers $headers -TimeoutSec 30
if ($release.tag_name -ne $releaseTag) { throw 'Pinned GitHub tag mismatch' }
$asset = @($release.assets | Where-Object { $_.name -eq $assetName }) | Select-Object -First 1
if (-not $asset -or $asset.browser_download_url -ne $assetUrl -or
    [int64]$asset.size -ne $assetSize -or
    $asset.digest -ne ('sha256:' + $sha256)) {
    throw 'Pinned official asset metadata mismatch'
}

& docker image inspect 'ubuntu:22.04' --format '{{.Id}}' *> $null
if ($LASTEXITCODE -ne 0) { throw 'Ubuntu 22.04 base image not cached. No image pull attempted.' }
& docker image inspect $image --format '{{.Id}}' *> $null
if ($LASTEXITCODE -eq 0) { throw "Refusing to replace an existing image: $image" }

$root = Join-Path ([IO.Path]::GetTempPath()) ('nv13-codex-build-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($root) | Out-Null
try {
    $archive = Join-Path $root $assetName
    Write-Output ('NV13_VERIFIED_RELEASE_URL=' + $assetUrl)
    $curlArgs = @(
        '-fL', '--proto', '=https', '--proto-redir', '=https',
        '--tlsv1.2', '--connect-timeout', '25', '--max-time', '260',
        '--retry', '2', '--silent', '--show-error',
        '--output', $archive, $assetUrl
    )
    & curl.exe @curlArgs
    if ($LASTEXITCODE -ne 0) { throw 'Official Codex archive download failed' }
    if ((Get-Item -LiteralPath $archive).Length -ne $assetSize) { throw 'Asset size mismatch' }
    $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $sha256) { throw 'Official GitHub SHA256 mismatch' }
    Write-Output ('NV13_GITHUB_RELEASE_SHA256_OK=' + $actual)

    $entries = @(& tar.exe -tzf $archive)
    if ($LASTEXITCODE -ne 0 -or $entries.Count -ne 1 -or $entries[0] -ne 'codex-x86_64-unknown-linux-musl') {
        throw 'Unexpected release tar entries'
    }
    & tar.exe -xzf $archive -C $root
    if ($LASTEXITCODE -ne 0) { throw 'Cannot extract release archive' }
    Move-Item -LiteralPath (Join-Path $root 'codex-x86_64-unknown-linux-musl') -Destination (Join-Path $root 'codex')
    Remove-Item -LiteralPath $archive -Force
    Write-Output ('NV13_LINUX_BINARY_SHA256=' + (Get-FileHash -LiteralPath (Join-Path $root 'codex') -Algorithm SHA256).Hash.ToLowerInvariant())

    $dockerfile = @'
FROM ubuntu:22.04
LABEL org.opencontainers.image.source="https://github.com/openai/codex"
LABEL org.opencontainers.image.version="0.162.1"
LABEL org.opencontainers.image.description="NV13 offline Codex CLI security fixture only"
COPY --chmod=0555 codex /usr/local/bin/codex
USER 65534:65534
ENV HOME=/nonexistent CODEX_HOME=/nonexistent
ENTRYPOINT ["/usr/local/bin/codex"]
'@
    [IO.File]::WriteAllText((Join-Path $root 'Dockerfile'), $dockerfile)
    Write-Output ('NV13_OFFLINE_DOCKER_BUILD_START=' + $image)
    & docker build --pull=false --network=none --no-cache --tag $image $root
    if ($LASTEXITCODE -ne 0) { throw 'Offline Docker build failed' }
    $info = @(& docker image inspect $image --format 'id={{.Id}} size={{.Size}} os={{.Os}} arch={{.Architecture}} user={{.Config.User}}')
    if ($LASTEXITCODE -ne 0) { throw 'Image inspect failed' }
    Write-Output ('NV13_INSTALLED_TEST_IMAGE=' + $image)
    Write-Output ('NV13_IMAGE_INFO=' + ($info -join ' '))
    Write-Output 'NV13_OFFICIAL_RELEASE_OFFLINE_BUILD_PASS'
}
finally {
    $parent = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($root)).TrimEnd('\')
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
    if ($parent.Equals($temp, [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($root).StartsWith('nv13-codex-build-', [StringComparison]::Ordinal) -and
        (Test-Path -LiteralPath $root -PathType Container)) {
        Remove-Item -LiteralPath $root -Force -Recurse
    }
    Write-Output 'NV13_DOWNLOAD_AND_BUILD_CONTEXT_CLEANED'
}
