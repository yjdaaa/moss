# NV13 synthetic model gateway fixture image (does not reach any model API).
# Only Ubuntu APT-signed packages; no user files or Windows configuration changes.
[CmdletBinding()]
param()
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$tag = 'moss-nv13-fake-api-gateway:jammy'
$root = Join-Path ([IO.Path]::GetTempPath()) ('nv13-echo-build-' + [Guid]::NewGuid().ToString('N'))
& docker image inspect $tag --format '{{.Id}}' *> $null
if ($LASTEXITCODE -eq 0) {
    $v = @(& docker image inspect $tag --format '{{index .Config.Labels "nv13.test.role"}}')
    if ($LASTEXITCODE -ne 0 -or ($v -join '') -ne 'synthetic-model-proxy-only') {
        throw 'Existing image has unexpected provenance; refuse reuse'
    }
    Write-Output ('NV13_FAKE_GATEWAY_IMAGE_ALREADY_EXISTS=' + $tag)
    return
}
& docker image inspect 'ubuntu:22.04' --format '{{.Id}}' *> $null
if ($LASTEXITCODE -ne 0) { throw 'Ubuntu base not available offline' }
[IO.Directory]::CreateDirectory($root) | Out-Null
try {
    $dockerfile = @'
FROM ubuntu:22.04
LABEL nv13.test.role="synthetic-model-proxy-only"
RUN set -eu; \
    apt-get -o Acquire::AllowInsecureRepositories=false -o Acquire::AllowDowngradeToInsecureRepositories=false update; \
    DEBIAN_FRONTEND=noninteractive apt-get -o APT::Get::AllowUnauthenticated=false -y --no-install-recommends install netcat-openbsd; \
    dpkg-query -W netcat-openbsd; \
    rm -rf /var/lib/apt/lists/*
USER 65534:65534
ENTRYPOINT ["/bin/sh"]
'@
    [IO.File]::WriteAllText((Join-Path $root 'Dockerfile'), $dockerfile)
    Write-Output 'NV13_BUILD_SIGNED_UBUNTU_NETCAT_TEST_GATEWAY'
    & docker build --pull=false --network=default --tag $tag $root
    if ($LASTEXITCODE -ne 0) { throw 'Synthetic gateway container image build failed' }
    $info = @(& docker image inspect $tag --format 'id={{.Id}} user={{.Config.User}} size={{.Size}}')
    if ($LASTEXITCODE -ne 0) { throw 'New gateway image cannot be inspected' }
    Write-Output ('NV13_FAKE_MODEL_GATEWAY_IMAGE=' + $tag)
    Write-Output ('NV13_FAKE_MODEL_GATEWAY_IMAGE_DETAILS=' + ($info -join ' '))
}
finally {
    $parent = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($root)).TrimEnd('\')
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
    if ($parent.Equals($temp, [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($root).StartsWith('nv13-echo-build-', [StringComparison]::Ordinal) -and
        (Test-Path -LiteralPath $root -PathType Container)) {
        Remove-Item -LiteralPath $root -Recurse -Force
    }
    Write-Output 'NV13_MOCK_PROXY_BUILD_CONTEXT_CLEANED'
}
