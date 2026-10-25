#!/usr/bin/env bash
# macOS release bundle builder (.app and .dmg), bundling NV18 moss-voice-local sidecar + GPL sources/notices by default.
# Pass --without-voice-sidecar only when debugging bare desktop packaging without the local voice helper.
# Does not install, mount disk images, touch user runtime data, or connect to hardware.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS host required" >&2
  exit 1
fi

WITH_SIDECAR=1
for arg in "$@"; do
  case "$arg" in
    --with-voice-sidecar)
      WITH_SIDECAR=1
      ;;
    --without-voice-sidecar)
      WITH_SIDECAR=0
      ;;
    -h|--help)
      echo "Usage: $0 [--with-voice-sidecar|--without-voice-sidecar]"
      echo "Env:   MOSS_MACOS_TARGET=aarch64-apple-darwin|x86_64-apple-darwin|universal-apple-darwin"
      echo "       APPLE_SIGNING_IDENTITY='<Developer ID Application: ...>' (optional; defaults to ad-hoc '-')"
      exit 0
      ;;
    *)
      echo "Unknown option: $arg" >&2
      exit 2
      ;;
  esac
done

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DESKTOP="$REPO/apps/desktop/src-tauri"
HELPER_ROOT="$REPO/apps/moss-voice-local"
TARGET_DIR="$REPO/target"

HOST_ARCH="$(uname -m)"
if [[ "${MOSS_MACOS_TARGET:-}" != "" ]]; then
  TRIPLE="$MOSS_MACOS_TARGET"
elif [[ "$HOST_ARCH" == "arm64" ]]; then
  TRIPLE="aarch64-apple-darwin"
else
  TRIPLE="x86_64-apple-darwin"
fi

can_exec_target() {
  local target="$1"
  if [[ "$target" == "universal-apple-darwin" ]]; then
    return 0
  elif [[ "$HOST_ARCH" == "arm64" && "$target" == "aarch64-apple-darwin" ]]; then
    return 0
  elif [[ "$HOST_ARCH" == "x86_64" && "$target" == "x86_64-apple-darwin" ]]; then
    return 0
  fi
  return 1
}

require_file() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    echo "Missing required file: $path" >&2
    exit 1
  fi
}

copy_new() {
  local from="$1"
  local to="$2"
  require_file "$from"
  mkdir -p "$(dirname "$to")"
  if [[ -e "$to" ]]; then
    echo "Refusing to replace staged file: $to" >&2
    exit 1
  fi
  cp "$from" "$to"
}

pushd "$REPO/frontend" >/dev/null
npm run build
popd >/dev/null

if [[ "$WITH_SIDECAR" -eq 0 ]]; then
  pushd "$DESKTOP" >/dev/null
  cargo tauri build --target "$TRIPLE"
  popd >/dev/null
  APP_BUNDLE="$TARGET_DIR/$TRIPLE/release/bundle/macos/MOSS.app"
  codesign --verify --deep --strict "$APP_BUNDLE"
  echo "MACOS_BUNDLE_DONE_WITHOUT_SIDECAR target=$TRIPLE app=$APP_BUNDLE"
  exit 0
fi

BUILD_ID="$(uuidgen | tr '[:upper:]' '[:lower:]' | tr -d '-')"
REL_RESOURCE="bundle-resources/nv18-$BUILD_ID/voice-local"
RESOURCE="$DESKTOP/$REL_RESOURCE"
SRC="$RESOURCE/source"
BIN_REL="binaries/nv18-$BUILD_ID/moss-voice-local"
BIN="$DESKTOP/binaries/nv18-$BUILD_ID/moss-voice-local-$TRIPLE"
RUN_DIR="$TARGET_DIR/nv18-$BUILD_ID"
OVERLAY="$RUN_DIR/overlay.json"
MANIFEST="$RUN_DIR/release-manifest.json"

for dir in "$RESOURCE" "$RUN_DIR" "$(dirname "$BIN")"; do
  if [[ -e "$dir" ]]; then
    echo "Build staging collision: $dir" >&2
    exit 1
  fi
done
mkdir -p "$RESOURCE" "$RUN_DIR" "$(dirname "$BIN")"

echo "NV18_BUILDING_HELPER ($TRIPLE)"
if [[ "$TRIPLE" == "universal-apple-darwin" ]]; then
  for sub_triple in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release --manifest-path "$HELPER_ROOT/Cargo.toml" --target "$sub_triple" --target-dir "$TARGET_DIR"
    test -s "$TARGET_DIR/$sub_triple/release/moss-voice-local"
    copy_new "$TARGET_DIR/$sub_triple/release/moss-voice-local" "$(dirname "$BIN")/moss-voice-local-$sub_triple"
    chmod 755 "$(dirname "$BIN")/moss-voice-local-$sub_triple"
  done
  lipo -create -output "$BIN" \
    "$TARGET_DIR/aarch64-apple-darwin/release/moss-voice-local" \
    "$TARGET_DIR/x86_64-apple-darwin/release/moss-voice-local"
else
  cargo build --release --manifest-path "$HELPER_ROOT/Cargo.toml" --target "$TRIPLE" --target-dir "$TARGET_DIR"
  HELPER_BIN="$TARGET_DIR/$TRIPLE/release/moss-voice-local"
  test -s "$HELPER_BIN"
  copy_new "$HELPER_BIN" "$BIN"
fi
chmod 755 "$BIN"
if can_exec_target "$TRIPLE"; then
  HELPER_VERSION="$("$BIN" --version)"
  echo "NV18_HELPER_VERSION=$HELPER_VERSION"
fi

# Only NEW unique resource directories are created. Existing workspace files are never overwritten.
for file in Cargo.toml Cargo.lock build.rs COPYING README.md; do
  copy_new "$HELPER_ROOT/$file" "$SRC/apps/moss-voice-local/$file"
done
cp -R "$HELPER_ROOT/src" "$SRC/apps/moss-voice-local/src"
copy_new "$REPO/crates/moss-voice-local-proto/Cargo.toml" "$SRC/crates/moss-voice-local-proto/Cargo.toml"
cp -R "$REPO/crates/moss-voice-local-proto/src" "$SRC/crates/moss-voice-local-proto/src"
copy_new "$REPO/Cargo.lock" "$SRC/Cargo.lock"
for file in COPYING README.md THIRD_PARTY_NOTICES.md; do
  copy_new "$HELPER_ROOT/$file" "$RESOURCE/$file"
done
if [[ -f "$HELPER_ROOT/native-source-evidence.json" ]]; then
  copy_new "$HELPER_ROOT/native-source-evidence.json" "$RESOURCE/native-source-evidence.json"
  cp -R "$HELPER_ROOT/native-license-evidence" "$RESOURCE/native-license-evidence"
fi
if [[ -f "$HELPER_ROOT/rust-source-evidence.json" ]]; then
  copy_new "$HELPER_ROOT/rust-source-evidence.json" "$RESOURCE/rust-source-evidence.json"
  cp -R "$HELPER_ROOT/rust-license-evidence" "$RESOURCE/rust-license-evidence"
fi

python3 - "$OVERLAY" "$BIN_REL" "$REL_RESOURCE" "${APPLE_SIGNING_IDENTITY:-}" <<'PY'
import json, sys

overlay_path, bin_rel, rel_resource, signing_id = sys.argv[1:5]
bundle = {
    "externalBin": [bin_rel],
    "resources": {f"{rel_resource}/": "licenses/voice-local/"},
}
if signing_id:
    bundle["macOS"] = {
        "signingIdentity": signing_id,
        "hardenedRuntime": True,
    }
with open(overlay_path, "w", encoding="utf-8") as fp:
    json.dump({"bundle": bundle}, fp, indent=2, ensure_ascii=False)
PY

STAMP_FILE="$RUN_DIR/.build-started"
touch "$STAMP_FILE"

pushd "$DESKTOP" >/dev/null
cargo tauri build --target "$TRIPLE" --config "$OVERLAY"
popd >/dev/null

APP_BUNDLE="$TARGET_DIR/$TRIPLE/release/bundle/macos/MOSS.app"
APP_DESKTOP="$APP_BUNDLE/Contents/MacOS/moss-desktop"
APP_HELPER="$APP_BUNDLE/Contents/MacOS/moss-voice-local"
APP_COPYING="$APP_BUNDLE/Contents/Resources/licenses/voice-local/COPYING"
APP_MAIN_RS="$APP_BUNDLE/Contents/Resources/licenses/voice-local/source/apps/moss-voice-local/src/main.rs"

test -x "$APP_DESKTOP"
test -x "$APP_HELPER"
require_file "$APP_COPYING"
require_file "$APP_MAIN_RS"

if can_exec_target "$TRIPLE"; then
  "$APP_HELPER" --version >/dev/null
fi
codesign --verify --deep --strict "$APP_BUNDLE"

DMG_DIR="$TARGET_DIR/$TRIPLE/release/bundle/dmg"
DMG_FILE="$(python3 - "$DMG_DIR" "$STAMP_FILE" <<'PY'
import pathlib, sys

dmg_dir = pathlib.Path(sys.argv[1])
stamp_mtime = pathlib.Path(sys.argv[2]).stat().st_mtime - 5.0
candidates = sorted(
    (p for p in dmg_dir.glob("*.dmg") if p.is_file() and p.stat().st_mtime >= stamp_mtime),
    key=lambda p: p.stat().st_mtime,
    reverse=True,
)
if not candidates:
    raise SystemExit("No fresh .dmg found in " + str(dmg_dir))
print(candidates[0])
PY
)"

python3 - "$REPO" "$DESKTOP" "$SRC" "$APP_DESKTOP" "$APP_HELPER" "$DMG_FILE" "$MANIFEST" "$TRIPLE" <<'PY'
import datetime, hashlib, json, pathlib, re, sys

repo = pathlib.Path(sys.argv[1])
desktop = pathlib.Path(sys.argv[2])
src = pathlib.Path(sys.argv[3])
app_desktop = pathlib.Path(sys.argv[4])
app_helper = pathlib.Path(sys.argv[5])
dmg_file = pathlib.Path(sys.argv[6])
manifest = pathlib.Path(sys.argv[7])
triple = sys.argv[8]

def sha256_file(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fp:
        for chunk in iter(lambda: fp.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()

def file_meta(path: pathlib.Path) -> dict:
    return {
        "name": path.name,
        "bytes": path.stat().st_size,
        "sha256": sha256_file(path),
    }

proto_src = (repo / "crates/moss-voice-local-proto/src/lib.rs").read_text(encoding="utf-8")
db_src = (repo / "crates/laser-runtime/src/db.rs").read_text(encoding="utf-8")
proto_match = re.search(r"pub const PROTOCOL_VERSION:\s*u32\s*=\s*(\d+)", proto_src)
schema_match = re.search(r"pub const SCHEMA_VERSION:\s*i64\s*=\s*(\d+)", db_src)
if not proto_match or not schema_match:
    raise SystemExit("Cannot verify protocol or schema version")

hash_lines = []
for item in sorted((p for p in src.rglob("*") if p.is_file()), key=lambda p: str(p)):
    rel = item.relative_to(src).as_posix()
    hash_lines.append(f"{rel}={sha256_file(item)}")
source_hash = hashlib.sha256("\n".join(hash_lines).encode("utf-8")).hexdigest()

tauri_conf = json.loads((desktop / "tauri.conf.json").read_text(encoding="utf-8"))
release = {
    "product_version": tauri_conf["version"],
    "target": triple,
    "helper_protocol": int(proto_match.group(1)),
    "db_schema": int(schema_match.group(1)),
    "source_tree_sha256": source_hash,
    "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "files": {
        "desktop": file_meta(app_desktop),
        "helper": file_meta(app_helper),
        "installer": file_meta(dmg_file),
    },
    "validation": "built_codesign_verified_not_installed_or_notarized",
}
manifest.write_text(json.dumps(release, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
print("NV18_APP=" + str(app_desktop.parent.parent.parent))
print("NV18_BUNDLE=" + str(dmg_file))
print("NV18_MANIFEST=" + str(manifest))
print("NV18_DMG_SHA256=" + release["files"]["installer"]["sha256"])
print("NV18_SOURCE_TREE_SHA256=" + source_hash)
PY

echo "MACOS_SIDECAR_BUNDLE_DONE target=$TRIPLE staging=$RUN_DIR"
