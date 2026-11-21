#!/usr/bin/env bash
# Removes rebuildable build caches on macOS / Linux (aligned with clean-build-cache.ps1,
# plus orphan/stale .rcgu.o pruning for macOS split-debuginfo=unpacked leftovers).
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

if [[ ! -d "$TARGET_DIR" || -L "$TARGET_DIR" ]]; then
  echo "No target/ directory; nothing to clean."
  exit 0
fi

# 0. Clean build artifacts older than 3 days via cargo-sweep when installed
if command -v cargo-sweep >/dev/null 2>&1; then
  for ws_dir in "$REPO_DIR" "$REPO_DIR/apps/moss-voice-local"; do
    if [[ "$ws_dir" == "$REPO_DIR" ]]; then
      [[ -d "$ws_dir/target" && ! -L "$ws_dir/target" ]] || continue
    else
      [[ -d "$ws_dir/target" && ! -L "$REPO_DIR/apps" && ! -L "$ws_dir" && ! -L "$ws_dir/target" ]] || continue
    fi
    pushd "$ws_dir" >/dev/null
    if [[ "$DRY_RUN" -eq 1 ]]; then
      cargo sweep --time 3 --dry-run || true
    else
      cargo sweep --time 3 || true
    fi
    popd >/dev/null
  done
fi

# 1–5. Use python3 for deterministic cross-Unix mtime/size/regex pruning
python3 - "$REPO_DIR" "$TARGET_DIR" "$DRY_RUN" <<'PY'
import os
import pathlib
import re
import shutil
import subprocess
import sys
import time

repo = pathlib.Path(sys.argv[1])
target = pathlib.Path(sys.argv[2])
dry_run = sys.argv[3] == "1"
cutoff = time.time() - 3 * 86400
freed = 0
removed: set[pathlib.Path] = set()

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

def has_symlink_component(p: pathlib.Path) -> bool:
    try:
        rel = p.relative_to(repo)
    except ValueError:
        return True
    cur = repo
    for part in rel.parts:
        cur = cur / part
        if cur.is_symlink():
            return True
    return False

def is_already_removed(p: pathlib.Path) -> bool:
    return p in removed or any(parent in removed for parent in p.parents)

def remove_cache(p: pathlib.Path, *, quiet: bool = False) -> int:
    global freed
    if is_already_removed(p) or has_symlink_component(p) or (not p.exists() and not p.is_symlink()):
        return 0
    size = dir_or_file_size(p)
    removed.add(p)
    freed += size
    if dry_run:
        if not quiet:
            print(f"[dry-run] delete {size / (1024 * 1024):.1f} MB: {p}")
        return size
    try:
        if p.is_dir() and not p.is_symlink():
            shutil.rmtree(p, ignore_errors=True)
        else:
            p.unlink(missing_ok=True)
    except OSError:
        pass
    return size

def safe_mtime(p: pathlib.Path) -> float:
    try:
        return p.stat().st_mtime
    except OSError:
        return 0.0

def is_real_dir(p: pathlib.Path) -> bool:
    return not has_symlink_component(p) and p.is_dir()

def is_real_file(p: pathlib.Path) -> bool:
    return not has_symlink_component(p) and p.is_file()

# 1. Incremental caches: remove >3 days old, keep only newest session dir per crate prefix
prefix_re = re.compile(r"^(.+)-[0-9a-z]+$")
for inc in list(target.glob("**/incremental")):
    if not is_real_dir(inc) or len(inc.relative_to(target).parts) > 4:
        continue
    dirs = [d for d in inc.iterdir() if is_real_dir(d)]
    for d in dirs:
        if safe_mtime(d) < cutoff:
            remove_cache(d)
    dirs = [d for d in inc.iterdir() if not is_already_removed(d) and is_real_dir(d)]
    groups: dict[str, list[pathlib.Path]] = {}
    for d in dirs:
        m = prefix_re.match(d.name)
        key = m.group(1) if m else d.name
        groups.setdefault(key, []).append(d)
    for group in groups.values():
        group.sort(key=safe_mtime, reverse=True)
        for stale in group[1:]:
            remove_cache(stale)
        if group and not is_already_removed(group[0]) and is_real_dir(group[0]):
            sessions = sorted(
                [s for s in group[0].iterdir() if not is_already_removed(s) and is_real_dir(s)],
                key=safe_mtime,
                reverse=True,
            )
            for stale_sess in sessions[1:]:
                remove_cache(stale_sess)

for child in list(target.iterdir()):
    if is_real_dir(child) and (child.name == "tmp" or child.name.startswith("codex-")):
        remove_cache(child)

# 2. Stray sub-crate target/ dirs (exempt independent workspace apps/moss-voice-local/target)
independent_targets = {repo / "apps/moss-voice-local/target"}
for pattern in ("crates/*/target", "apps/*/target", "apps/desktop/src-tauri/target", "tools/*/target"):
    for stray in repo.glob(pattern):
        if stray in independent_targets:
            continue
        if is_real_dir(stray):
            remove_cache(stray)

# 3. Android Gradle intermediates and debug APKs
android_build = repo / "apps/desktop/src-tauri/gen/android/app/build"
if is_real_dir(android_build / "intermediates"):
    remove_cache(android_build / "intermediates")
for dbg in android_build.glob("outputs/apk/*/debug"):
    if is_real_dir(dbg):
        remove_cache(dbg)

# 4. NV18 staging copies & old installer/temp artifacts
for staging in ("apps/desktop/src-tauri/bundle-resources", "apps/desktop/src-tauri/binaries"):
    base = repo / staging
    if is_real_dir(base):
        for d in base.glob("nv18-*"):
            if is_real_dir(d):
                remove_cache(d)

nv18_runs = sorted([d for d in target.glob("nv18-*") if is_real_dir(d)], key=safe_mtime, reverse=True)
for stale_run in nv18_runs[1:]:
    remove_cache(stale_run)

for lic in target.glob("*/release/licenses"):
    if is_real_dir(lic):
        remove_cache(lic)

for dmg_dir in target.glob("*/release/bundle/dmg"):
    if is_real_dir(dmg_dir):
        dmgs = sorted([f for f in dmg_dir.glob("*.dmg") if is_real_file(f)], key=safe_mtime, reverse=True)
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
    if not is_real_file(mf):
        continue
    for m in name_re.finditer(mf.read_text(encoding="utf-8", errors="ignore")):
        cname = m.group(1).replace("-", "_")
        crate_names.add(cname)
        crate_names.add(f"{cname}_lib")
    tdir = mf.parent / "tests"
    if is_real_dir(tdir):
        for tf in tdir.glob("*.rs"):
            if is_real_file(tf):
                test_names.add(tf.stem.replace("-", "_"))

art_re = re.compile(r"^(lib)?(.+)-([0-9a-f]{16})(\..+)?$")
deps_dirs = [d for d in target.glob("**/deps") if is_real_dir(d) and len(d.relative_to(target).parts) <= 4]
for deps_dir in deps_dirs:
    buckets: dict[tuple[str, str, str], list[pathlib.Path]] = {}
    for f in deps_dir.iterdir():
        if not is_real_file(f) or f.name.endswith(".rcgu.o"):
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

# 6. Orphan / stale .rcgu.o files in deps/ (e.g. macOS split-debuginfo=unpacked leftovers)
hash_tail_re = re.compile(r"^(.+)-([0-9a-f]{16})$")
MACHO_MAGICS = {
    b"\xfe\xed\xfa\xce",
    b"\xce\xfa\xed\xfe",
    b"\xfe\xed\xfa\xcf",
    b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe",
    b"\xbe\xba\xfe\xca",
}
ar_bin = shutil.which("ar")
nm_bin = shutil.which("nm") if sys.platform == "darwin" else None

def is_macho_binary(p: pathlib.Path) -> bool:
    try:
        with p.open("rb") as fh:
            return fh.read(4) in MACHO_MAGICS
    except OSError:
        return False

def collect_oso_refs(profile_dir: pathlib.Path, deps_dir: pathlib.Path) -> tuple[bool, set[str]]:
    refs: set[str] = set()
    if sys.platform != "darwin":
        return True, refs
    # Fail closed on macOS when nm is unavailable so live OSO-referenced .rcgu.o files are never pruned.
    if not nm_bin:
        return False, refs
    candidates: list[pathlib.Path] = []
    for entry in profile_dir.iterdir():
        if is_already_removed(entry) or not is_real_file(entry):
            continue
        if (entry.suffix == ".dylib" or (entry.suffix == "" and os.access(entry, os.X_OK))) and is_macho_binary(entry):
            candidates.append(entry)
    for entry in deps_dir.iterdir():
        if is_already_removed(entry) or not is_real_file(entry):
            continue
        if entry.suffix == ".dylib" and is_macho_binary(entry):
            candidates.append(entry)
    for entry in candidates:
        proc = subprocess.run(
            [nm_bin, "-pa", str(entry)],
            capture_output=True,
            text=True,
            errors="replace",
            check=False,
        )
        if proc.returncode != 0:
            return False, refs
        for line in proc.stdout.splitlines():
            if " OSO " in line:
                part = line.split(" OSO ", 1)[1].strip()
                if part:
                    refs.add(os.path.basename(part))
    return True, refs

def collect_archive_members(archives: list[pathlib.Path]) -> tuple[bool, set[str]]:
    members: set[str] = set()
    if not ar_bin or not archives:
        return False, members
    for arc in archives:
        proc = subprocess.run(
            [ar_bin, "-t", str(arc)],
            capture_output=True,
            text=True,
            errors="replace",
            check=False,
        )
        if proc.returncode != 0:
            return False, members
        for line in proc.stdout.splitlines():
            item = line.strip()
            if item.endswith(".o"):
                members.add(item)
    return bool(members), members

for deps_dir in deps_dirs:
    rcgu_files: list[pathlib.Path] = []
    live_hashes: dict[str, set[str]] = {}
    surviving_unhashed_files: set[str] = set()

    for f in deps_dir.iterdir():
        if is_already_removed(f) or not is_real_file(f):
            continue
        if f.name.endswith(".rcgu.o"):
            rcgu_files.append(f)
            continue
        if f.suffix in (".o", ".d"):
            continue
        head = f.name.split(".", 1)[0]
        m = hash_tail_re.match(head)
        if m:
            stem, h = m.group(1), m.group(2)
            live_hashes.setdefault(stem, set()).add(h)
            if stem.startswith("lib") and len(stem) > 3:
                live_hashes.setdefault(stem[3:], set()).add(h)
        else:
            surviving_unhashed_files.add(f.name)

    if not rcgu_files:
        continue

    profile_dir = deps_dir.parent
    oso_ok, oso_refs = collect_oso_refs(profile_dir, deps_dir)
    unhashed_members_cache: dict[str, tuple[str, set[str]]] = {}

    def unhashed_state(stem: str) -> tuple[str, set[str]]:
        cached = unhashed_members_cache.get(stem)
        if cached is not None:
            return cached
        archives = [
            p
            for p in (
                deps_dir / f"lib{stem}.a",
                deps_dir / f"lib{stem}.rlib",
                profile_dir / f"lib{stem}.a",
                profile_dir / f"lib{stem}.rlib",
            )
            if not is_already_removed(p) and is_real_file(p)
        ]
        if archives:
            ok, members = collect_archive_members(archives)
            res = ("archived", members) if ok else ("keep", set())
        else:
            has_peer = any(
                is_real_file(base / name) and not is_already_removed(base / name)
                for base in (deps_dir, profile_dir)
                for name in (stem, f"lib{stem}.dylib", f"lib{stem}.so")
            ) or any(name.startswith(f"lib{stem}.") for name in surviving_unhashed_files)
            res = ("keep", set()) if has_peer else ("no_owner", set())
        unhashed_members_cache[stem] = res
        return res

    pruned_rcgu_cnt = 0
    pruned_rcgu_bytes = 0
    for f in rcgu_files:
        if f.name in oso_refs:
            continue
        head = f.name.split(".", 1)[0]
        m = hash_tail_re.match(head)
        if m:
            stem, h = m.group(1), m.group(2)
            if h not in live_hashes.get(stem, set()) and oso_ok:
                pruned_rcgu_bytes += remove_cache(f, quiet=True)
                pruned_rcgu_cnt += 1
        elif oso_ok:
            state, members = unhashed_state(head)
            if (state == "archived" and f.name not in members) or (
                state == "no_owner" and safe_mtime(f) < cutoff
            ):
                pruned_rcgu_bytes += remove_cache(f, quiet=True)
                pruned_rcgu_cnt += 1

    if dry_run and pruned_rcgu_cnt:
        print(
            f"[dry-run] delete {pruned_rcgu_bytes / (1024 * 1024):.1f} MB "
            f"({pruned_rcgu_cnt} orphan .rcgu.o files) in {deps_dir}"
        )

mode = "Would reclaim" if dry_run else "Reclaimed"
print(f"{mode} {freed / (1024 * 1024):.1f} MB from {target}")
PY
