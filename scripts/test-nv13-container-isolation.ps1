# NV13: no-model, no-network Docker Desktop Linux isolation fixture.
# Uses ONLY a cached, known Ubuntu image and a new temporary directory.
# It never mounts MOSS projects, Docker socket, user profiles, or credentials.
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$docker = Get-Command docker.exe -ErrorAction SilentlyContinue
if (-not $docker) { throw 'Docker CLI is not installed; NV13 container test cannot run.' }

# Do not implicitly download an image, contact a registry, or install software.
& docker image inspect 'ubuntu:22.04' --format '{{.Id}}' *> $null
if ($LASTEXITCODE -ne 0) {
    throw 'ubuntu:22.04 is not cached. No image download was attempted.'
}

$id = [Guid]::NewGuid().ToString('N')
$root = Join-Path ([IO.Path]::GetTempPath()) ('nv13-docker-' + $id)
$work = Join-Path $root 'workspace'
$name = 'nv13-fixture-' + $id.Substring(0, 16)
$listener = $null

try {
    [IO.Directory]::CreateDirectory($work) | Out-Null
    [IO.File]::WriteAllText((Join-Path $work 'inside.txt'), 'NV13_SAFE_CANARY')
    [IO.File]::WriteAllText((Join-Path $root 'private-canary.txt'), 'NV13_PRIVATE_CANARY')

    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    $pending = $listener.AcceptTcpClientAsync()

    $fixture = @'
set -eu
test "$(cat /workspace/inside.txt)" = "NV13_SAFE_CANARY"
echo NV13_WORKSPACE_CANARY_READ_PASS

if [ -e /workspace/../private-canary.txt ]; then
  echo NV13_OUTSIDE_HOST_FILE_VISIBLE; exit 20
fi
if cat /workspace/../private-canary.txt >/dev/null 2>&1; then
  echo NV13_OUTSIDE_HOST_FILE_READABLE; exit 21
fi
echo NV13_OUTSIDE_HOST_FILE_NOT_MOUNTED_PASS

if touch /workspace/unexpected-write >/dev/null 2>&1; then
  echo NV13_WORKSPACE_WRITABLE; exit 22
fi
if touch /tmp/unexpected-write >/dev/null 2>&1; then
  echo NV13_CONTAINER_ROOTFS_WRITABLE; exit 23
fi
echo NV13_READ_ONLY_MOUNTS_PASS

if [ -e /var/run/docker.sock ]; then
  echo NV13_HOST_DOCKER_SOCKET_VISIBLE; exit 24
fi
echo NV13_HOST_DOCKER_SOCKET_ABSENT_PASS

if awk 'NR>1 && $2=="00000000" {found=1} END {exit !found}' /proc/net/route; then
  echo NV13_UNEXPECTED_DEFAULT_ROUTE; exit 25
fi
echo NV13_NO_DEFAULT_NETWORK_ROUTE_PASS

if /bin/bash -c 'echo test >/dev/tcp/127.0.0.1/PORT_PLACEHOLDER' >/dev/null 2>&1; then
  echo NV13_HOST_LOOPBACK_CONNECTED; exit 26
fi
echo NV13_HOST_LOOPBACK_DENIED_PASS

test -r /sys/fs/cgroup/pids.max
test "$(cat /sys/fs/cgroup/pids.max)" = "8"
echo NV13_EIGHT_PROCESS_CGROUP_PASS

if env | grep -E '^(MOSS_|MCP_|OPENAI_API_KEY=|ANTHROPIC_API_KEY=)' >/dev/null; then
  echo NV13_SENSITIVE_ENV_EXPOSED; exit 27
fi
echo NV13_SENSITIVE_ENV_NOT_INHERITED_PASS

grep -q '^CapEff:[[:space:]]*0000000000000000' /proc/self/status
grep -q '^Seccomp:[[:space:]]*2' /proc/self/status
echo NV13_CAPABILITIES_AND_SECCOMP_PASS
echo NV13_DOCKER_ISOLATION_FIXTURE_PASS
'@
    $fixture = $fixture.Replace('PORT_PLACEHOLDER', [string]$port)
    $args = @(
        'run', '--rm', '--pull', 'never', '--name', $name,
        '--network', 'none', '--read-only', '--cap-drop', 'ALL',
        '--security-opt', 'no-new-privileges',
        '--pids-limit', '8', '--memory', '256m', '--cpus', '0.5',
        '--user', '65534:65534',
        '--mount', ('type=bind,source=' + $work + ',target=/workspace,readonly'),
        '--workdir', '/workspace', 'ubuntu:22.04', '/bin/sh', '-c', $fixture
    )
    $output = @(& docker @args 2>&1)
    $status = $LASTEXITCODE
    foreach ($line in $output) { Write-Output $line }
    if ($status -ne 0 -or $output -notcontains 'NV13_DOCKER_ISOLATION_FIXTURE_PASS') {
        throw ('NV13 Docker isolation fixture failed (exit=' + $status + ').')
    }
    if ($pending.IsCompletedSuccessfully) {
        $connection = $pending.GetAwaiter().GetResult()
        $connection.Dispose()
        throw 'NV13 test container connected to Windows loopback: isolation failed.'
    }
    if ([IO.File]::ReadAllText((Join-Path $root 'private-canary.txt')) -ne 'NV13_PRIVATE_CANARY') {
        throw 'NV13 host canary unexpectedly changed.'
    }
    Write-Output 'NV13_HOST_CANARY_UNCHANGED_PASS'
}
finally {
    if ($listener) { $listener.Stop() }
    # A random, task-owned container only; do not touch unrelated containers.
    $present = @(& docker ps -a --filter ('name=^/' + $name + '$') --format '{{.Names}}' 2>$null)
    if ($present -contains $name) { & docker rm -f $name *> $null }
    $expectedParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
    $parent = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($root)).TrimEnd('\')
    if ($parent.Equals($expectedParent, [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($root).StartsWith('nv13-docker-', [StringComparison]::Ordinal) -and
        (Test-Path -LiteralPath $root -PathType Container)) {
        Remove-Item -LiteralPath $root -Recurse -Force
    }
    Write-Output 'NV13_OWN_TEMP_RESOURCES_CLEANED'
}
