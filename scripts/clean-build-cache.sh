#!/usr/bin/env bash
# Removes rebuildable build caches on macOS / Linux (aligned with clean-build-cache.ps1).
# Usage: bash scripts/clean-build-cache.sh [--dry-run]
set -euo pipefail

DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --dry-run|-n) DRY_RUN=1 ;;
    *)
      echo "Unknown option: $arg (supported: --dry-run)" >&2
      exit 2
      ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_DIR="$REPO_DIR/target"

if [[ ! -d "$TARGET_DIR" ]]; then
  echo "No target/ directory; nothing to clean."
  exit 0
fi

# 0. Clean build artifacts older than 3 days via cargo-sweep when installed
if command -v cargo-sweep >/dev/null 2>&1; then
  pushd "$REPO_DIR" >/dev/null
  if [[ "$DRY_RUN" -eq 1 ]]; then
    cargo sweep --time 3 --dry-run || true
  else
    cargo sweep --time 3 || true
  fi
  popd >/dev/null
fi

# 1–5. Use python3 for deterministic cross-Unix mtime/size/regex pruning
python3 - "$REPO_DIR" "$TARGET_DIR" "$DRY_RUN" <<'PY'
import os
import pathlib
import re
import shutil
import sys
import time

repo = pathlib.Path(sys.argv[1])
target = pathlib.Path(sys.argv[2])
dry_run = sys.argv[3] == "1"
cutoff = time.time() - 3 * 86400
freed = 0

def dir_or_file_size(p: pathlib.Path) -> int:
    try:
        if p.is_symlink() or p.is_file():
            return p.lstat().st_size
    except OSError:
        return 0
    total = 0
    for root, _, files in os.walk(p, followlinks=False):
        for f in files:
            fp = pathlib.Path(root) / f
            try:
                total += fp.lstat().st_size
            except OSError:
                pass
    return total

def remove_cache(p: pathlib.Path) -> None:
    global freed
    if not p.exists() and not p.is_symlink():
        return
    size = dir_or_file_size(p)
    freed += size
    if dry_run:
        print(f"[dry-run] delete {size / (1024 * 1024):.1f} MB: {p}")
        return
    try:
        if p.is_dir() and not p.is_symlink():
            shutil.rmtree(p, ignore_errors=True)
        else:
            p.unlink(missing_ok=True)
    except OSError:
        pass

def safe_mtime(p: pathlib.Path) -> float:
    try:
        return p.stat().st_mtime
    except OSError:
        return 0.0

# 1. Incremental caches: remove >3 days old, keep only newest session dir per crate prefix
prefix_re = re.compile(r"^(.+)-[0-9a-z]+$")
for inc in list(target.glob("**/incremental")):
    if not inc.is_dir() or len(inc.relative_to(target).parts) > 4:
        continue
    dirs = [d for d in inc.iterdir() if d.is_dir()]
    for d in dirs:
        if safe_mtime(d) < cutoff:
            remove_cache(d)
    dirs = [d for d in inc.iterdir() if d.is_dir()]
    groups: dict[str, list[pathlib.Path]] = {}
    for d in dirs:
        m = prefix_re.match(d.name)
        key = m.group(1) if m else d.name
        groups.setdefault(key, []).append(d)
    for group in groups.values():
        group.sort(key=safe_mtime, reverse=True)
        for stale in group[1:]:
            remove_cache(stale)
        if group and group[0].is_dir():
            sessions = sorted([s for s in group[0].iterdir() if s.is_dir()], key=safe_mtime, reverse=True)
            for stale_sess in sessions[1:]:
                remove_cache(stale_sess)

for child in list(target.iterdir()):
    if child.is_dir() and (child.name == "tmp" or child.name.startswith("codex-")):
        remove_cache(child)

# 2. Stray sub-crate target/ dirs
for pattern in ("crates/*/target", "apps/*/target", "apps/desktop/src-tauri/target", "tools/*/target"):
    for stray in repo.glob(pattern):
        if stray.is_dir():
            remove_cache(stray)

# 3. Android Gradle intermediates and debug APKs
android_build = repo / "apps/desktop/src-tauri/gen/android/app/build"
remove_cache(android_build / "intermediates")
for dbg in android_build.glob("outputs/apk/*/debug"):
    if dbg.is_dir():
        remove_cache(dbg)

# 4. NV18 staging copies & old installer/temp artifacts
for staging in ("apps/desktop/src-tauri/bundle-resources", "apps/desktop/src-tauri/binaries"):
    base = repo / staging
    if base.is_dir():
        for d in base.glob("nv18-*"):
            if d.is_dir():
                remove_cache(d)

nv18_runs = sorted([d for d in target.glob("nv18-*") if d.is_dir()], key=safe_mtime, reverse=True)
for stale_run in nv18_runs[1:]:
    remove_cache(stale_run)

for lic in target.glob("*/release/licenses"):
    if lic.is_dir():
        remove_cache(lic)

for dmg_dir in target.glob("*/release/bundle/dmg"):
    if dmg_dir.is_dir():
        dmgs = sorted([f for f in dmg_dir.glob("*.dmg") if f.is_file()], key=safe_mtime, reverse=True)
        for stale_dmg in dmgs[1:]:
            remove_cache(stale_dmg)

# 5. Stale hashed artifacts of this repo's crates in deps/ (newest kept per kind)
crate_names: set[str] = set()
test_names: set[str] = set()
manifests = (
    list(repo.glob("crates/*/Cargo.toml"))
    + list(repo.glob("apps/*/Cargo.toml"))
    + [repo / "apps/desktop/src-tauri/Cargo.toml"]
)
name_re = re.compile(r'^name\s*=\s*"([^"]+)"', re.M)
for mf in manifests:
    if not mf.is_file():
        continue
    for m in name_re.finditer(mf.read_text(encoding="utf-8", errors="ignore")):
        cname = m.group(1).replace("-", "_")
        crate_names.add(cname)
        crate_names.add(f"{cname}_lib")
    tdir = mf.parent / "tests"
    if tdir.is_dir():
        for tf in tdir.glob("*.rs"):
            test_names.add(tf.stem.replace("-", "_"))

art_re = re.compile(r"^(lib)?(.+)-([0-9a-f]{16})(\..+)?$")
for deps_dir in list(target.glob("**/deps")):
    if not deps_dir.is_dir() or len(deps_dir.relative_to(target).parts) > 4:
        continue
    buckets: dict[tuple[str, str, str], list[pathlib.Path]] = {}
    for f in deps_dir.iterdir():
        if not f.is_file():
            continue
        m = art_re.match(f.name)
        if not m:
            continue
        lib_prefix, stem, _hash, ext = m.group(1) or "", m.group(2), m.group(3), m.group(4) or ""
        if stem in crate_names or (not lib_prefix and stem in test_names and ext in ("", ".d")):
            buckets.setdefault((lib_prefix, stem, ext), []).append(f)
    for files in buckets.values():
        files.sort(key=safe_mtime, reverse=True)
        for stale_file in files[1:]:
            remove_cache(stale_file)

mode = "Would reclaim" if dry_run else "Reclaimed"
print(f"{mode} {freed / (1024 * 1024):.1f} MB from {target}")
PY
