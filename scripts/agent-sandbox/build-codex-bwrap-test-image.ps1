# NV13 optional inner sandbox compatibility fixture.
# Installs Ubuntu-signed bubblewrap package ONLY into a derived Docker image.
# Does not alter Windows, Docker daemon settings, existing images, Codex login
# or MOSS production. The image has no authorization to run model requests.
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$parentTag = 'moss-nv13-codex-test:0.162.1'
$parentId = 'sha256:baa32539eac9ecca7831bac5848c0ecab8a0a84986c5822a4d371fffa7d435ac'
$testTag = 'moss-nv13-codex-bwrap-test:0.162.1-jammy'

$actual = @(& docker image inspect $parentTag --format '{{.Id}}' 2>&1)
if ($LASTEXITCODE -ne 0 -or ($actual -join '') -ne $parentId) {
    throw 'Pinned Codex image is missing or unexpectedly changed'
}
& docker image inspect $testTag --format '{{.Id}}' *> $null
if ($LASTEXITCODE -eq 0) {
    throw 'Test image tag already exists. Refusing to replace existing image.'
}

$id = [Guid]::NewGuid().ToString('N')
$root = Join-Path ([IO.Path]::GetTempPath()) ('nv13-bwrap-build-' + $id)
[IO.Directory]::CreateDirectory($root) | Out-Null
try {
    $file = @'
FROM moss-nv13-codex-test:0.162.1
LABEL org.opencontainers.image.source="https://github.com/openai/codex"
LABEL org.opencontainers.image.version="0.162.1"
LABEL nv13.test.role="isolated-inner-bwrap-only"
USER root
RUN set -eu; \
    apt-get -o Acquire::AllowInsecureRepositories=false -o Acquire::AllowDowngradeToInsecureRepositories=false update; \
    DEBIAN_FRONTEND=noninteractive apt-get -o APT::Get::AllowUnauthenticated=false -y --no-install-recommends install bubblewrap; \
    dpkg-query -W bubblewrap; \
    rm -rf /var/lib/apt/lists/*
USER 65534:65534
ENV HOME=/nonexistent CODEX_HOME=/nonexistent
'@
    [IO.File]::WriteAllText((Join-Path $root 'Dockerfile'), $file)
    Write-Output ('NV13_BWRAP_BUILD_FROM_PINNED_IMAGE=' + $parentId)
    Write-Output 'NV13_ORIGINAL_UBUNTU_APT_REPOS_WITH_GPG_SIGNATURE_CHECKS_ONLY'
    # Build stage may access Ubuntu signed APT; runtime tests are fully offline.
    # No host project files, secrets or Docker socket are mounted.
    & docker build --pull=false --network=default --no-cache --tag $testTag $root
    if ($LASTEXITCODE -ne 0) { throw 'Official Ubuntu bubblewrap build failed' }
    $image = @(& docker image inspect $testTag --format 'id={{.Id}} os={{.Os}} arch={{.Architecture}} user={{.Config.User}} bytes={{.Size}}')
    if ($LASTEXITCODE -ne 0) { throw 'Built test image missing' }
    Write-Output ('NV13_INSTALLED_BWRAP_TEST_IMAGE=' + $testTag)
    Write-Output ('NV13_BWRAP_IMAGE_INFO=' + ($image -join ' '))
    Write-Output 'NV13_BWRAP_DERIVED_TEST_IMAGE_BUILD_PASS'
}
finally {
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
    $parent = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($root)).TrimEnd('\')
    if ($parent.Equals($temp, [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($root).StartsWith('nv13-bwrap-build-', [StringComparison]::Ordinal) -and
        (Test-Path -LiteralPath $root -PathType Container)) {
        Remove-Item -LiteralPath $root -Recurse -Force
    }
    Write-Output 'NV13_BWRAP_BUILD_CONTEXT_CLEANED'
}
