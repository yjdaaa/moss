# NV13 OFFLINE bubblewrap / nested Codex sandbox diagnostic, not approval.
# Uses only synthetic workspace, no credentials or model calls. Never grants
# Docker SYS_ADMIN, privileged, unconfined seccomp, host filesystem or network.
[CmdletBinding()]
param([string]$ReportPath)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$image = 'moss-nv13-codex-bwrap-test:0.162.1-jammy'
$imageId = 'sha256:7e90320dc50afccbd1d036a21e7ee020365182cf599f5b75ac0e839360b150c6'
$id = [Guid]::NewGuid().ToString('N')
$root = Join-Path ([IO.Path]::GetTempPath()) ('nv13-bwrap-smoke-' + $id)
$work = Join-Path $root 'workspace'
$prefix = 'nv13-bwrap-' + $id.Substring(0, 16)
$names = @()
$report=[ordered]@{schema=1;tested_at=(Get-Date).ToUniversalTime().ToString('o');image_id=$imageId;cli_version='0.162.1';status='blocked';reason='runtime_unavailable';userns_exit=$null;sandbox_exit=$null;negative_probes='not_run';production_ready=$false}
try {
    if($ReportPath -and (Test-Path -LiteralPath $ReportPath)){throw 'Refusing to overwrite existing sandbox receipt'}
    $server=@(& docker version --format '{{.Server.Version}}' 2>&1)
    if($LASTEXITCODE -ne 0){throw 'NV13 Docker runtime unavailable; no sandbox test ran'}
    $info = @(& docker image inspect $image --format '{{.Id}}|{{.Os}}/{{.Architecture}}|{{.Config.User}}|{{index .Config.Labels "nv13.test.role"}}' 2>&1)
    if ($LASTEXITCODE -ne 0 -or $info.Count -ne 1 -or
        $info[0] -ne ($imageId+'|linux/amd64|65534:65534|isolated-inner-bwrap-only')) {
        $report.reason='pinned_image_unavailable_or_changed'
        throw 'Unexpected bubblewrap fixture image'
    }
    $image=$imageId
    [IO.Directory]::CreateDirectory($work) | Out-Null
    [IO.File]::WriteAllText((Join-Path $work 'inside.txt'), 'NV13_NESTED_BWRAP_INSIDE')
    [IO.File]::WriteAllText((Join-Path $root 'private.txt'), 'NV13_BWRAP_OUTSIDE')

    $restricted = @(
        '--rm', '--pull', 'never', '--network', 'none', '--read-only',
        '--tmpfs', '/tmp:rw,noexec,nosuid,size=16m,mode=1777',
        '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
        '--pids-limit', '8', '--memory', '256m', '--cpus', '0.5',
        '--user', '65534:65534', '--env', 'HOME=/workspace',
        '--env', 'CODEX_HOME=/workspace', '--env', 'TMPDIR=/tmp',
        '--mount', ('type=bind,source=' + $work + ',target=/workspace,readonly'),
        '--workdir', '/workspace'
    )
    $name = $prefix + '-version'
    $names += $name
    $bwrap = @(& docker run @restricted --name $name --entrypoint /usr/bin/bwrap $image --version 2>&1)
    if ($LASTEXITCODE -ne 0 -or ($bwrap -join ' ') -notmatch 'bubblewrap') {
        throw ('bubblewrap is not installed or cannot report version: ' + ($bwrap -join ' '))
    }
    Write-Output ('NV13_UBUNTU_BWRAP_BINARY_VERSION=' + ($bwrap -join ' '))

    $name = $prefix + '-userns'
    $names += $name
    $namespace = @(& docker run @restricted --name $name --entrypoint /usr/bin/unshare $image --user --map-root-user /bin/true 2>&1)
    $namespaceExit = $LASTEXITCODE
    $report.userns_exit=$namespaceExit
    Write-Output ('NV13_NONPRIVILEGED_USER_NAMESPACE_EXIT=' + $namespaceExit)
    foreach ($line in ($namespace | Select-Object -First 5)) { Write-Output ('NV13_USERNS_DIAGNOSTIC=' + [string]$line) }

    $name = $prefix + '-sandbox'
    $names += $name
    $output = @(& docker run @restricted --name $name $image sandbox -P ':read-only' -C '/workspace' -- /bin/sh -c 'cat /workspace/inside.txt' 2>&1)
    $sandboxExit = $LASTEXITCODE
    $report.sandbox_exit=$sandboxExit
    Write-Output ('NV13_REAL_CODEX_NESTED_SANDBOX_EXIT=' + $sandboxExit)
    foreach ($line in ($output | Select-Object -Last 16)) {
        Write-Output ('NV13_NESTED_SANDBOX_LOG=' + [string]$line)
    }
    if ($sandboxExit -eq 0 -and $output -contains 'NV13_NESTED_BWRAP_INSIDE') {
        Write-Output 'NV13_REAL_CODEX_INNER_SANDBOX_READ_PASS'
        # A successful read alone does NOT authorize a real Codex model call.
        Write-Output 'NV13_INNER_SANDBOX_ONLY_BASIC_READ_VERIFIED'
        $report.status='basic_read_only';$report.reason='full_negative_acceptance_not_run'
    } else {
        Write-Output 'NV13_REAL_CODEX_INNER_SANDBOX_BLOCKED'
        Write-Output 'NV13_PRODUCTION_AGENT_MUST_REMAIN_DISABLED'
        $report.reason=if($namespaceExit -ne 0){'unprivileged_user_namespace_denied'}else{'codex_inner_sandbox_failed'}
    }
    if ([IO.File]::ReadAllText((Join-Path $root 'private.txt')) -ne 'NV13_BWRAP_OUTSIDE') {
        throw 'Host synthetic private canary changed'
    }
    Write-Output 'NV13_SYNTHETIC_PRIVATE_CANARY_UNCHANGED'
    # A diagnostic must never exit zero while the requested sandbox is
    # blocked, or when only a basic positive read (not acceptance) succeeded.
    throw ('NV13 acceptance incomplete: '+$report.reason)
}
finally {
    foreach ($name in $names) {
        $running = @(& docker ps -a --filter ('name=^/' + $name + '$') --format '{{.Names}}' 2>$null)
        if ($running -contains $name) { & docker rm -f $name *> $null }
    }
    $parent = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($root)).TrimEnd('\')
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
    if ($parent.Equals($temp, [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($root).StartsWith('nv13-bwrap-smoke-', [StringComparison]::Ordinal) -and
        (Test-Path -LiteralPath $root -PathType Container)) {
        $owned=@(Get-Item -LiteralPath $root -Force)+@(Get-ChildItem -LiteralPath $root -Recurse -Force)
        if(@($owned|Where-Object{$_.Attributes -band [IO.FileAttributes]::ReparsePoint}).Count -gt 0){throw 'Refusing sandbox cleanup through reparse points'}
        Remove-Item -LiteralPath $root -Force -Recurse
    }
    Write-Output 'NV13_ONLY_OWN_BWRAP_TEST_FILES_AND_CONTAINERS_CLEANED'
    if($ReportPath -and -not(Test-Path -LiteralPath $ReportPath)){
        [IO.File]::WriteAllText([IO.Path]::GetFullPath($ReportPath),($report|ConvertTo-Json -Depth 4))
    }
}
