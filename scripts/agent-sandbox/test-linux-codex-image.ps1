# NV13 actual official Linux Codex CLI OFFLINE checks. No account or model calls.
# This script requires a previously checksum-verified local test image.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^moss-nv13-codex-test:[0-9]+\.[0-9]+\.[0-9]+$')]
    [string]$Image
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$id = [Guid]::NewGuid().ToString('N')
$root = Join-Path ([IO.Path]::GetTempPath()) ('nv13-codex-smoke-' + $id)
$work = Join-Path $root 'workspace'
$prefix = 'nv13-codex-' + $id.Substring(0, 16)
$names = @()
$listener = $null
$expectedImageId='sha256:baa32539eac9ecca7831bac5848c0ecab8a0a84986c5822a4d371fffa7d435ac'
try {
    $actualId=@(& docker image inspect $Image --format '{{.Id}}' 2>&1)
    if($LASTEXITCODE -ne 0 -or $actualId.Count -ne 1 -or $actualId[0] -ne $expectedImageId){throw 'Pinned official Codex fixture image missing or changed'}
    $details = @(& docker image inspect $Image --format '{{.Os}}/{{.Architecture}}|{{.Config.User}}|{{json .Config.Labels}}' 2>&1)
    if ($LASTEXITCODE -ne 0 -or $details.Count -ne 1) { throw 'Local Linux test image missing' }
    $parts = $details[0].Split('|', 3)
    if ($parts[0] -ne 'linux/amd64' -or $parts[1] -ne '65534:65534') { throw 'Test image wrong platform or user' }
    $labels = $parts[2] | ConvertFrom-Json
    if ($labels.'org.opencontainers.image.source' -ne 'https://github.com/openai/codex' -or
        $labels.'org.opencontainers.image.version' -ne '0.162.1') { throw 'Unexpected image provenance labels' }
    # Launch the inspected immutable ID, not a tag that may move afterwards.
    $runImage=$expectedImageId

    [IO.Directory]::CreateDirectory($work) | Out-Null
    [IO.File]::WriteAllText((Join-Path $work 'canary.txt'), 'NV13_WORKSPACE_CANARY')
    [IO.File]::WriteAllText((Join-Path $root 'private-canary.txt'), 'NV13_PRIVATE_CANARY')
    $limits = @(
        '--rm', '--pull', 'never', '--network', 'none',
        '--read-only', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
        '--pids-limit', '8', '--memory', '256m', '--cpus', '0.5',
        '--user', '65534:65534', '--env', 'HOME=/nonexistent', '--env', 'CODEX_HOME=/nonexistent',
        '--mount', ('type=bind,source=' + $work + ',target=/workspace,readonly'),
        '--workdir', '/workspace'
    )

    $name = $prefix + '-version'
    $names += $name
    $text = @(& docker run @limits --name $name $runImage --version 2>&1)
    # An immutable read-only CODEX_HOME intentionally has no aliases folder.
    # Codex may print a non-fatal alias warning; the actual CLI version must
    # still appear and exit 0. Never remove the no-credential HOME restriction.
    $hasVersion = (($text -join ' ') -match '(?<!\S)codex-cli 0\.162\.1(?!\S)')
    if ($LASTEXITCODE -ne 0 -or -not $hasVersion) {
        throw ('Offline codex --version check failed: ' + ($text -join ' '))
    }
    Write-Output 'NV13_REAL_LINUX_CODEX_VERSION=codex-cli 0.162.1'
    $name = $prefix + '-help'
    $names += $name
    $help = @(& docker run @limits --name $name $runImage --help 2>&1)
    if ($LASTEXITCODE -ne 0 -or ($help -join ' ') -notmatch '(?i)sandbox') { throw 'Codex --help check failed' }
    Write-Output 'NV13_REAL_LINUX_CODEX_HELP_PASS_NO_LOGIN'
    $name = $prefix + '-exec-help'
    $names += $name
    $execHelp = @(& docker run @limits --name $name $runImage exec --help 2>&1)
    if ($LASTEXITCODE -ne 0 -or ($execHelp -join ' ') -notmatch '(?i)sandbox|read-only') {
        throw 'Codex exec --help check failed'
    }
    Write-Output 'NV13_CODEX_EXEC_HELP_PASS_NO_MODEL'

    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    $pending = $listener.AcceptTcpClientAsync()
    $fixture = @'
set -eu
test "$(cat /workspace/canary.txt)" = "NV13_WORKSPACE_CANARY"
test -x /usr/local/bin/codex
echo NV13_ALLOWED_FILE_READ_PASS
if [ -e /workspace/../private-canary.txt ] || cat /workspace/../private-canary.txt >/dev/null 2>&1; then
 echo NV13_UNMOUNTED_PRIVATE_FILE_VISIBLE; exit 20
fi
echo NV13_UNMOUNTED_PRIVATE_FILE_NOT_VISIBLE_PASS
if touch /workspace/forbidden >/dev/null 2>&1 || touch /tmp/forbidden >/dev/null 2>&1; then
 echo NV13_WRITABLE_FILESYSTEM; exit 21
fi
echo NV13_FILESYSTEM_READONLY_PASS
if [ -e /var/run/docker.sock ]; then echo NV13_DOCKER_SOCKET_VISIBLE; exit 22; fi
echo NV13_NO_DOCKER_SOCKET_PASS
if awk 'NR>1 && $2=="00000000" {seen=1} END {exit !seen}' /proc/net/route; then
 echo NV13_DEFAULT_ROUTE_PRESENT; exit 23
fi
if /bin/bash -c 'echo hello >/dev/tcp/127.0.0.1/PORT_PLACEHOLDER' >/dev/null 2>&1; then
 echo NV13_WINDOWS_LOOPBACK_REACHABLE; exit 24
fi
echo NV13_NO_EXTERNAL_ROUTE_OR_HOST_LOOPBACK_PASS
test "$(cat /sys/fs/cgroup/pids.max)" = "8"
grep -q '^CapEff:[[:space:]]*0000000000000000' /proc/self/status
grep -q '^Seccomp:[[:space:]]*2' /proc/self/status
echo NV13_CGROUP_CAPABILITIES_SECCOMP_PASS
if env | grep -E '^(MOSS_|MCP_|OPENAI_API_KEY=|ANTHROPIC_API_KEY=)' >/dev/null; then
 echo NV13_PRIVATE_ENV_EXPOSED; exit 25
fi
test ! -e /nonexistent/.codex/auth.json
echo NV13_NO_ACCOUNT_FILES_OR_PRIVATE_ENV_PASS
echo NV13_OFFLINE_CODEX_CONTAINER_FIXTURE_PASS
'@
    $fixture = $fixture.Replace('PORT_PLACEHOLDER', [string]$port)
    $name = $prefix + '-fixture'
    $names += $name
    $results = @(& docker run @limits --name $name --entrypoint /bin/sh $runImage -c $fixture 2>&1)
    $code = $LASTEXITCODE
    foreach ($line in $results) { Write-Output $line }
    if ($code -ne 0 -or $results -notcontains 'NV13_OFFLINE_CODEX_CONTAINER_FIXTURE_PASS') {
        throw ('Offline container fixture failed, exit=' + $code)
    }
    if ($pending.IsCompletedSuccessfully) { throw 'Container reached Windows loopback listener' }
    if ([IO.File]::ReadAllText((Join-Path $root 'private-canary.txt')) -ne 'NV13_PRIVATE_CANARY') {
        throw 'Host private canary modified'
    }

    $name = $prefix + '-stop'
    $names += $name
    $started = @(& docker run --detach @limits --name $name --entrypoint /bin/sh $runImage -c 'sleep 60' 2>&1)
    if ($LASTEXITCODE -ne 0) { throw ('Fixture start failed: ' + ($started -join ' ')) }
    $stopped = @(& docker stop --time 2 $name 2>&1)
    if ($LASTEXITCODE -ne 0) { throw ('Fixture stop failed: ' + ($stopped -join ' ')) }
    $remaining = @(& docker ps -a --filter ('name=^/' + $name + '$') --format '{{.Names}}')
    if ($remaining -contains $name) { throw 'Stopped fixture still present' }
    Write-Output 'NV13_CONTAINER_STOP_AND_REMOVE_PASS'
    Write-Output 'NV13_LINUX_CODEX_OFFLINE_SMOKE_PASS'
}
finally {
    if ($listener) { $listener.Stop() }
    foreach ($name in $names) {
        $existing = @(& docker ps -a --filter ('name=^/' + $name + '$') --format '{{.Names}}' 2>$null)
        if ($existing -contains $name) { & docker rm -f $name *> $null }
    }
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
    $parent = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($root)).TrimEnd('\')
    if ($parent.Equals($temp, [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($root).StartsWith('nv13-codex-smoke-', [StringComparison]::Ordinal) -and
        (Test-Path -LiteralPath $root -PathType Container)) {
        Remove-Item -LiteralPath $root -Recurse -Force
    }
    Write-Output 'NV13_TEST_CONTAINERS_AND_TEMP_CANARIES_CLEANED'
}
