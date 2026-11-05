# Magpie-style 1-second local release trigger (pure ASCII for Windows PowerShell 5.1+ compatibility).
# Pushes a `vX.Y.Z` Git tag so `.github/workflows/release.yml` builds Windows / macOS / Android in parallel
# on GitHub's cloud runners and publishes the GitHub Release automatically without blocking your local PC.
param(
  [string]$Version = '',
  [switch]$AllowDirty,
  [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location $repo
try {
  if (-not $AllowDirty) {
    $dirty = @(git status --porcelain)
    if ($dirty.Count -gt 0) {
      throw "Working tree has uncommitted changes. Commit or stash first (or pass -AllowDirty)."
    }
  }

  $cleanVer = $Version.Trim()
  if ($cleanVer.StartsWith('v')) {
    $cleanVer = $cleanVer.Substring(1)
  }

  if ($cleanVer -eq '') {
    $latestTag = @(git tag --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-v:refname | Select-Object -First 1)
    if ($latestTag.Count -eq 0 -or -not $latestTag[0]) {
      $cleanVer = '0.1.0'
    } else {
      $m = [regex]::Match($latestTag[0], '^v(\d+)\.(\d+)\.(\d+)$')
      if (-not $m.Success) {
        throw ("Cannot auto-increment non-standard tag '" + $latestTag[0] + "'. Pass -Version X.Y.Z explicitly.")
      }
      $maj = [int]$m.Groups[1].Value
      $min = [int]$m.Groups[2].Value
      $pat = [int]$m.Groups[3].Value + 1
      $cleanVer = "$maj.$min.$pat"
    }
  }

  if ($cleanVer -notmatch '^\d+\.\d+\.\d+([.-][0-9A-Za-z.-]+)?$') {
    throw "Invalid semantic version: '$cleanVer' (expected X.Y.Z)"
  }

  $tag = "v$cleanVer"
  $existing = @(git tag --list $tag)
  if ($existing.Count -gt 0) {
    throw "Tag '$tag' already exists locally. Choose the next version."
  }

  $branch = (git rev-parse --abbrev-ref HEAD).Trim()
  $remoteUrl = (git remote get-url origin).Trim()
  $repoSlug = ''
  $slugMatch = [regex]::Match($remoteUrl, 'github\.com[:/]([^/]+/[^/.]+?)(?:\.git)?$')
  if ($slugMatch.Success) {
    $repoSlug = $slugMatch.Groups[1].Value
  }

  if ($DryRun) {
    Write-Output "[MOSS Release] DRY RUN: branch=$branch tag=$tag version=$cleanVer"
    exit 0
  }

  Write-Output "[MOSS Release] Pushing branch '$branch' to origin..."
  & git push origin $branch
  if ($LASTEXITCODE -ne 0) { throw "Failed to push branch '$branch'" }

  Write-Output "[MOSS Release] Creating and pushing tag '$tag'..."
  & git tag -a $tag -m "MOSS $tag"
  if ($LASTEXITCODE -ne 0) { throw "Failed to create tag '$tag'" }

  & git push origin $tag
  if ($LASTEXITCODE -ne 0) { throw "Failed to push tag '$tag'" }

  Write-Output "[MOSS Release] Tag '$tag' pushed in 1 step! Cloud runners are now building Windows + macOS + Android in parallel."
  if ($repoSlug -ne '') {
    Write-Output "[MOSS Release] Actions progress : https://github.com/$repoSlug/actions"
    Write-Output "[MOSS Release] Release page     : https://github.com/$repoSlug/releases/tag/$tag"
  }
} finally {
  Pop-Location
}
