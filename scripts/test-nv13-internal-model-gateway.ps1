# NV13 synthetic Docker internal-only API gateway test.
# Gateway is a fake HTTP echo fixture; NEVER connects to a model API or
# receives user authentication. Creates and removes its own Docker network.
[CmdletBinding()]
param()
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$clientImage = 'moss-nv13-codex-test:0.162.1'
$gatewayImage = 'moss-nv13-fake-api-gateway:jammy'
$id = [Guid]::NewGuid().ToString('N')
$network = 'nv13-offline-' + $id.Substring(0, 16)
$server = 'nv13-proxy-' + $id.Substring(0, 16)
$client = 'nv13-client-' + $id.Substring(0, 16)
$root = Join-Path ([IO.Path]::GetTempPath()) ('nv13-egress-test-' + $id)
$work = Join-Path $root 'workspace'
$listener = $null
$networkCreated = $false
$serverCreated = $false

try {
    foreach ($image in @($clientImage, $gatewayImage)) {
        & docker image inspect $image --format '{{.Id}}' *> $null
        if ($LASTEXITCODE -ne 0) { throw ('Required local image is unavailable: ' + $image) }
    }
    [IO.Directory]::CreateDirectory($work) | Out-Null
    [IO.File]::WriteAllText((Join-Path $work 'canary.txt'), 'NV13_ONLY_SYNTHETIC_WORKSPACE')
    [IO.File]::WriteAllText((Join-Path $root 'private.txt'), 'NV13_UNMOUNTED_PRIVATE')

    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    $pending = $listener.AcceptTcpClientAsync()

    & docker network create --internal --driver bridge --label 'nv13.test=offline-model-mock' $network | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Creating isolated Docker-only network failed' }
    $networkCreated = $true
    $internal = @(& docker network inspect $network --format '{{.Internal}}')
    if ($LASTEXITCODE -ne 0 -or ($internal -join '') -ne 'true') {
        throw 'Docker test network is not internal-only'
    }
    Write-Output 'NV13_DOCKER_INTERNAL_NETWORK_CONFIRMED'

    # Returns an artificial success string, never contacts external APIs.
    $mockResponse = @'
printf "HTTP/1.0 200 OK\r\n\r\nNV13_FAKE_MODEL_REPLY_ONLY\n" | nc -l -p 18080 -N
'@
    $serverFlags = @(
        'run', '--detach', '--rm', '--pull', 'never', '--name', $server,
        '--network', $network, '--network-alias', 'nv13-model-mock',
        '--read-only', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
        '--pids-limit', '8', '--memory', '128m', '--cpus', '0.25',
        '--user', '65534:65534', '--entrypoint', '/bin/sh',
        $gatewayImage, '-c', $mockResponse
    )
    $started = @(& docker @serverFlags 2>&1)
    if ($LASTEXITCODE -ne 0) { throw ('Fake proxy could not start: ' + ($started -join ' ')) }
    $serverCreated = $true
    Start-Sleep -Milliseconds 350

    $probe = @'
set -eu
test "$(cat /workspace/canary.txt)" = NV13_ONLY_SYNTHETIC_WORKSPACE
test ! -e /workspace/../private.txt
test ! -e /var/run/docker.sock
if touch /workspace/forbidden-write >/dev/null 2>&1; then exit 21; fi
if touch /tmp/forbidden-write >/dev/null 2>&1; then exit 22; fi
if awk 'NR>1 && $2=="00000000" {route=1} END {exit !route}' /proc/net/route; then
  echo NV13_EXTERNAL_DEFAULT_ROUTE_DETECTED
  exit 23
fi
if bash -c 'echo test >/dev/tcp/host.docker.internal/HOST_PORT' >/dev/null 2>&1; then
  echo NV13_UNEXPECTED_WINDOWS_HOST_ACCESS
  exit 24
fi
echo NV13_NO_EXTERNAL_ROUTE_OR_HOST_LOOPBACK_PASS
connected=0
for i in 1 2 3 4 5; do
  if exec 3<>/dev/tcp/nv13-model-mock/18080 2>/dev/null; then
    connected=1
    break
  fi
  sleep 0.2
done
test "$connected" = 1
printf 'GET /synthetic HTTP/1.0\r\nHost: nv13-model-mock\r\n\r\n' >&3
IFS= read -r first <&3
test "$first" = "$(printf 'HTTP/1.0 200 OK\r')"
IFS= read -r empty <&3
IFS= read -r payload <&3
test "$payload" = NV13_FAKE_MODEL_REPLY_ONLY
exec 3<&-
exec 3>&-
echo NV13_ONLY_SYNTHETIC_HTTP_REPLY_RECEIVED
test "$(cat /sys/fs/cgroup/pids.max)" = 8
grep -q '^CapEff:[[:space:]]*0000000000000000' /proc/self/status
grep -q '^Seccomp:[[:space:]]*2' /proc/self/status
if env | grep -E '^(MOSS_|MCP_|OPENAI_API_KEY=|ANTHROPIC_API_KEY=)' >/dev/null; then exit 25; fi
echo NV13_NETWORK_POLICY_NO_KEYS_PASS
echo NV13_SYNTHETIC_INTERNAL_API_PROXY_TEST_PASS
'@
    $probe = $probe.Replace('HOST_PORT', [string]$port)
    $clientFlags = @(
        'run', '--rm', '--pull', 'never', '--name', $client,
        '--network', $network,
        '--read-only', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
        '--pids-limit', '8', '--memory', '256m', '--cpus', '0.5',
        '--user', '65534:65534', '--env', 'HOME=/nonexistent',
        '--env', 'CODEX_HOME=/nonexistent',
        '--mount', ('type=bind,source=' + $work + ',target=/workspace,readonly'),
        '--workdir', '/workspace', '--entrypoint', '/bin/bash',
        $clientImage, '-c', $probe
    )
    $logs = @(& docker @clientFlags 2>&1)
    $code = $LASTEXITCODE
    foreach ($line in $logs) { Write-Output $line }
    if ($code -ne 0 -or $logs -notcontains 'NV13_SYNTHETIC_INTERNAL_API_PROXY_TEST_PASS') {
        throw ('Offline model API gateway test failed (exit=' + $code + ')')
    }
    if ($pending.IsCompletedSuccessfully) {
        $conn = $pending.GetAwaiter().GetResult()
        $conn.Dispose()
        throw 'Docker client connected to Windows test listener'
    }
    if ([IO.File]::ReadAllText((Join-Path $root 'private.txt')) -ne 'NV13_UNMOUNTED_PRIVATE') {
        throw 'Synthetic private host file changed'
    }
    Write-Output 'NV13_WINDOWS_HOST_TEST_LISTENER_NOT_CONTACTED'
    Write-Output 'NV13_HOST_SYNTHETIC_PRIVATE_FILE_UNCHANGED'
    Write-Output 'NV13_NO_MODEL_NO_LOGIN_NETWORK_FIXTURE_PASS'
}
finally {
    if ($listener) { $listener.Stop() }
    foreach ($name in @($client, $server)) {
        $active = @(& docker ps -a --filter ('name=^/' + $name + '$') --format '{{.Names}}' 2>$null)
        if ($active -contains $name) { & docker rm --force $name *> $null }
    }
    if ($networkCreated) {
        & docker network rm $network *> $null
        if ($LASTEXITCODE -ne 0) { Write-Output 'NV13_TEST_NETWORK_REQUIRES_MANUAL_REVIEW' }
    }
    $parent = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($root)).TrimEnd('\')
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
    if ($parent.Equals($temp, [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($root).StartsWith('nv13-egress-test-', [StringComparison]::Ordinal) -and
        (Test-Path -LiteralPath $root -PathType Container)) {
        Remove-Item -LiteralPath $root -Recurse -Force
    }
    Write-Output 'NV13_SYNTHETIC_NETWORK_AND_TEMP_CONTAINERS_CLEANED'
}
