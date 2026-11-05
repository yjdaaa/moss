#!/usr/bin/env bash
# macOS counterpart of scripts/quick-update-desktop.ps1 (2026-11-05):
# fast official release build of moss-desktop (cargo tauri build --no-bundle, no .app/.dmg packaging)
# and in-place patch of an existing MOSS.app without rebuilding the installer.
# Never kills a running MOSS (preserves I-7 emergency-stop shutdown order): new files are written next
# to the old ones and atomically renamed over them, so a running MOSS keeps its old inode until reopened.
# Usage:
#   bash scripts/quick-update-desktop.sh [--build-only] [--install-dir <path/to/MOSS.app>]
#       [--with-voice-helper] [--with-host] [--launch] [--skip-cache-sweep] [--dry-run]
#       [--version X.Y.Z]
# Pure ASCII. Does not touch ~/Library/Application Support/moss, hardware, or the network.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "[MOSS QuickUpdate] macOS host required (use quick-update-desktop.ps1 on Windows)" >&2
  exit 1
fi

BUILD_ONLY=0
INSTALL_DIR=""
WITH_HELPER=0
WITH_HOST=0
LAUNCH=0
SKIP_SWEEP=0
DRY_RUN=0
VERSION_OVERRIDE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --build-only) BUILD_ONLY=1 ;;
    --install-dir) INSTALL_DIR="${2:-}"; shift ;;
    --with-voice-helper) WITH_HELPER=1 ;;
    --with-host) WITH_HOST=1 ;;
    --launch) LAUNCH=1 ;;
    --skip-cache-sweep) SKIP_SWEEP=1 ;;
    --dry-run) DRY_RUN=1 ;;
    --version) VERSION_OVERRIDE="${2:-}"; shift ;;
    -h|--help) sed -n '2,11p' "$0"; exit 0 ;;
    *) echo "Unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAURI_DIR="$REPO/apps/desktop/src-tauri"
HELPER_MANIFEST="$REPO/apps/moss-voice-local/Cargo.toml"
PROTO_SOURCE="$REPO/crates/moss-voice-local-proto/src/lib.rs"
RELEASE_BIN="$REPO/target/release/moss-desktop"
HOST_BIN="$REPO/target/release/moss-host"
HELPER_BUILT="$REPO/target/release/moss-voice-local"

log() { printf '[MOSS QuickUpdate] %s\n' "$*"; }
run() { if [[ "$DRY_RUN" -eq 1 ]]; then log "DRY RUN: $*"; else "$@"; fi; }

sha256() { shasum -a 256 "$1" | awk '{print $1}'; }

# Match the executable path only (ps comm), never argv: argv matching gives false positives for any
# process whose arguments merely mention the bundle path (codesign, cp, shasum ...).
# No `grep -q`: with pipefail an early grep exit makes ps die of SIGPIPE and the check would read "not running".
moss_running() { ps -axo comm= | grep -E '/MOSS\.app/Contents/MacOS/moss-desktop$' >/dev/null; }

expected_protocol() {
  sed -n 's/.*pub const PROTOCOL_VERSION: *u32 *= *\([0-9][0-9]*\);.*/\1/p' "$PROTO_SOURCE" | head -1
}

binary_protocol() {
  local bin="$1"
  [[ -x "$bin" ]] || { echo "-1"; return; }
  local out
  out="$("$bin" --version 2>/dev/null || true)"
  if [[ "$out" =~ \(protocol\ ([0-9]+) ]]; then echo "${BASH_REMATCH[1]}"; else echo "-1"; fi
}

resolve_app() {
  if [[ -n "$INSTALL_DIR" ]]; then
    [[ -x "$INSTALL_DIR/Contents/MacOS/moss-desktop" ]] || {
      echo "Refusing to patch '$INSTALL_DIR': Contents/MacOS/moss-desktop not found." >&2; exit 1; }
    (cd "$INSTALL_DIR" && pwd)
    return
  fi
  local cand
  for cand in "/Applications/MOSS.app" "$HOME/Applications/MOSS.app"; do
    if [[ -x "$cand/Contents/MacOS/moss-desktop" ]]; then echo "$cand"; return; fi
  done
  echo ""
}

# Atomically replace $dst with $src: copy to a temp file in the same directory, ad-hoc sign it, then rename.
# A running process keeps the old (now unlinked) inode, so MOSS is never interrupted.
replace_binary() {
  local src="$1" dst="$2"
  local src_hash
  src_hash="$(sha256 "$src")"
  if [[ -f "$dst" && "$(sha256 "$dst")" == "$src_hash" ]]; then
    return 1
  fi
  local tmp="$dst.new-$$"
  run cp "$src" "$tmp"
  run chmod 755 "$tmp"
  run codesign --force --sign - "$tmp"
  run mv -f "$tmp" "$dst"
  return 0
}

APP=""
if [[ "$BUILD_ONLY" -eq 0 ]]; then
  APP="$(resolve_app)"
fi

if [[ -z "$VERSION_OVERRIDE" && -n "$APP" ]]; then
  VERSION_OVERRIDE="$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist" 2>/dev/null || true)"
fi
if [[ -n "$VERSION_OVERRIDE" ]]; then
  if [[ ! "$VERSION_OVERRIDE" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
    echo "Invalid version: $VERSION_OVERRIDE" >&2; exit 2
  fi
  export MOSS_RELEASE_VERSION="$VERSION_OVERRIDE"
  log "Embedding version $VERSION_OVERRIDE (matches the installed MOSS.app unless --version is given)"
fi

EXPECTED_PROTO="$(expected_protocol)"
[[ -n "$EXPECTED_PROTO" ]] || { echo "Unable to parse PROTOCOL_VERSION from $PROTO_SOURCE" >&2; exit 1; }

# Voice helper: reuse the installed or previously built helper when its protocol matches; otherwise rebuild.
HELPER_SOURCE=""
if [[ "$WITH_HELPER" -eq 0 ]]; then
  for cand in "${APP:+$APP/Contents/MacOS/moss-voice-local}" "$HELPER_BUILT"; do
    [[ -n "$cand" ]] || continue
    if [[ "$(binary_protocol "$cand")" == "$EXPECTED_PROTO" ]]; then HELPER_SOURCE="$cand"; break; fi
  done
fi
if [[ "$WITH_HELPER" -eq 1 || -z "$HELPER_SOURCE" ]]; then
  log "Building release moss-voice-local (protocol $EXPECTED_PROTO) ..."
  run cargo build --release --manifest-path "$HELPER_MANIFEST" --target-dir "$REPO/target"
  if [[ "$DRY_RUN" -eq 0 && "$(binary_protocol "$HELPER_BUILT")" != "$EXPECTED_PROTO" ]]; then
    echo "Built helper protocol mismatch at $HELPER_BUILT (expected $EXPECTED_PROTO)" >&2; exit 1
  fi
  HELPER_SOURCE="$HELPER_BUILT"
fi

STARTED_AT="$(date +%s)"
log "Running cargo tauri build --no-bundle (frontend/dist + release moss-desktop, skips .app/.dmg packaging) ..."
if cargo tauri --version >/dev/null 2>&1; then
  (cd "$TAURI_DIR" && run cargo tauri build --no-bundle)
else
  log "cargo-tauri not found; falling back to npm run build + cargo build --features tauri/custom-protocol"
  (cd "$REPO/frontend" && run npm run build)
  (cd "$REPO" && run cargo build --release -p moss-desktop --features tauri/custom-protocol)
fi

if [[ "$WITH_HOST" -eq 1 ]]; then
  log "Building release moss-host ..."
  (cd "$REPO" && run cargo build --release -p moss-host)
fi

if [[ "$DRY_RUN" -eq 0 ]]; then
  [[ -s "$RELEASE_BIN" ]] || { echo "Release binary missing after build: $RELEASE_BIN" >&2; exit 1; }
  if [[ "$(stat -f %m "$RELEASE_BIN")" -lt $((STARTED_AT - 2)) ]]; then
    log "Note: Cargo reused the up-to-date release binary (no source changes detected)."
  fi
  log "Built release moss-desktop ($(du -h "$RELEASE_BIN" | awk '{print $1}'), sha256=$(sha256 "$RELEASE_BIN"))"
fi

RUNNING_DURING_PATCH=0
if [[ "$BUILD_ONLY" -eq 0 ]]; then
  if [[ -z "$APP" ]]; then
    log "No installed MOSS.app found in /Applications or ~/Applications; fresh binary is at target/release/moss-desktop."
    log "Install once from the DMG (see docs/developer/packaging-install.md section 3.9), then re-run to patch in place."
  else
    moss_running && RUNNING_DURING_PATCH=1
    MACOS_DIR="$APP/Contents/MacOS"
    CHANGED=0
    if replace_binary "$RELEASE_BIN" "$MACOS_DIR/moss-desktop"; then
      CHANGED=1
      log "Patched $MACOS_DIR/moss-desktop"
    else
      log "Installed moss-desktop already identical; skipped"
    fi
    if [[ -n "$HELPER_SOURCE" && "$HELPER_SOURCE" != "$MACOS_DIR/moss-voice-local" ]]; then
      if replace_binary "$HELPER_SOURCE" "$MACOS_DIR/moss-voice-local"; then
        CHANGED=1
        log "Patched voice helper (protocol $EXPECTED_PROTO) -> $MACOS_DIR/moss-voice-local"
      fi
    fi
    # Re-seal the bundle only when something was replaced (ad-hoc, same identity '-' as tauri.conf.json).
    # Re-signing rewrites the main executable, which by then is the new (not running) inode.
    if [[ "$CHANGED" -eq 1 ]]; then
      run codesign --force --sign - --preserve-metadata=entitlements,requirements,flags "$APP"
    else
      RUNNING_DURING_PATCH=0
    fi
    if [[ "$DRY_RUN" -eq 0 ]]; then
      codesign --verify --deep --strict "$APP"
      log "Signature verified: $APP (sha256 moss-desktop=$(sha256 "$MACOS_DIR/moss-desktop"))"
    fi
  fi
fi

if [[ "$SKIP_SWEEP" -eq 0 && "$DRY_RUN" -eq 0 ]] && command -v cargo-sweep >/dev/null 2>&1; then
  (cd "$REPO" && cargo sweep --time 3 >/dev/null 2>&1 || true)
fi

if [[ "$RUNNING_DURING_PATCH" -eq 1 ]]; then
  log "MOSS is currently open (left untouched); quit MOSS (Cmd+Q) and reopen to use the patched build."
fi

if [[ "$LAUNCH" -eq 1 && "$DRY_RUN" -eq 0 ]]; then
  if moss_running; then
    echo "MOSS is running; quit it first (never force-killed, to preserve 0x18 emergency-stop order)." >&2
    exit 1
  fi
  if [[ -n "$APP" ]]; then open "$APP"; else open "$RELEASE_BIN"; fi
fi
