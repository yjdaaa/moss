# Build the Windows installer that bundles the voice helper. Never install or operate hardware.
# Kept as the historical entry point; the real builder uses per-run unique staging and never
# deletes or overwrites an existing bundle-resources/voice-local (see docs/developer/packaging-install.md).
# The old in-place implementation (recursive cleanup) was removed; see git history if needed.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$safeBuilder = Join-Path $PSScriptRoot 'voice-helper/build-sidecar-installer.ps1'
if (-not (Test-Path -LiteralPath $safeBuilder -PathType Leaf)) {
  throw 'Missing non-destructive voice helper installer builder; refusing to continue.'
}
& $safeBuilder
