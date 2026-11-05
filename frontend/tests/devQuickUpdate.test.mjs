import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const readRepo = (rel) => readFileSync(new URL(`../../${rel}`, import.meta.url));
const readRepoText = (rel) => readRepo(rel).toString("utf8");

test("dev-desktop.ps1, quick-update-desktop.ps1, and clean-build-cache.ps1 stay pure ASCII and preserve I-7 shutdown safety", () => {
  for (const script of ["scripts/dev-desktop.ps1", "scripts/quick-update-desktop.ps1", "scripts/clean-build-cache.ps1"]) {
    const bytes = readRepo(script);
    const badIndex = bytes.findIndex((byte) => byte > 0x7f);
    assert.equal(badIndex, -1, `${script} contains non-ASCII byte at offset ${badIndex}`);
  }

  const quick = readRepoText("scripts/quick-update-desktop.ps1");
  assert.match(quick, /--no-bundle/);
  assert.match(quick, /Assert-MossNotRunning/);
  assert.doesNotMatch(quick, /Stop-Process[^\r\n]*(moss-desktop|moss-voice-local)/i, "must never force-kill running MOSS or voice helper");
  assert.doesNotMatch(quick, /taskkill[^\r\n]*(moss-desktop|moss-voice-local)/i, "must never force-kill running MOSS or voice helper");
});

test("quick-update, dev-desktop, and start.bat verify PROTOCOL_VERSION and sync moss-voice-local.exe next to moss-desktop.exe", () => {
  const protoSrc = readRepoText("crates/moss-voice-local-proto/src/lib.rs");
  const protoMatch = protoSrc.match(/pub\s+const\s+PROTOCOL_VERSION\s*:\s*u32\s*=\s*(\d+)\s*;/);
  assert.ok(protoMatch && Number(protoMatch[1]) >= 4, "PROTOCOL_VERSION must be parsable from moss-voice-local-proto");

  const quick = readRepoText("scripts/quick-update-desktop.ps1");
  assert.match(quick, /Get-ExpectedVoiceHelperProtocol/);
  assert.match(quick, /Get-BinaryVoiceHelperProtocol/);
  assert.match(quick, /target\/release\/moss-voice-local\.exe/);
  assert.match(quick, /Sync-HelperBinary\s+\$activeHelperSource\s+\$releaseHelperExe/);

  const dev = readRepoText("scripts/dev-desktop.ps1");
  assert.match(dev, /Ensure-DevVoiceHelper/);
  assert.match(dev, /Test-MossViteHttp/);
  assert.match(dev, /Test-CommandLineMatchesFrontendDir/);
  assert.match(dev, /Test-MossViteProcess/);
  assert.match(dev, /Test-VerifiedMossViteOrigin/);
  assert.match(dev, /target\/debug\/moss-voice-local\.exe/);
  assert.doesNotMatch(dev, /Stop-Process[^\r\n]*(moss-desktop|moss-voice-local)/i);
  assert.doesNotMatch(dev, /taskkill[^\r\n]*(moss-desktop|moss-voice-local)/i);

  const startBat = readRepoText("scripts/start.bat");
  assert.match(startBat, /PROTOCOL_VERSION/);
  assert.match(startBat, /moss-voice-local\.exe/);
});

test("macOS quick-update-desktop.sh stays ASCII, patches MOSS.app atomically, re-signs, and never kills MOSS", () => {
  const bytes = readRepo("scripts/quick-update-desktop.sh");
  const badIndex = bytes.findIndex((byte) => byte > 0x7f);
  assert.equal(badIndex, -1, `quick-update-desktop.sh contains non-ASCII byte at offset ${badIndex}`);

  const quick = readRepoText("scripts/quick-update-desktop.sh");
  assert.match(quick, /--no-bundle/);
  assert.match(quick, /PROTOCOL_VERSION/);
  assert.match(quick, /binary_protocol/);
  // Atomic replace: write next to the target, sign, then rename over it (running MOSS keeps the old inode)
  assert.match(quick, /\.new-\$\$/);
  assert.match(quick, /mv -f "\$tmp" "\$dst"/);
  assert.match(quick, /codesign --verify --deep --strict/);
  assert.match(quick, /MOSS_RELEASE_VERSION/);
  // Running detection must match the executable path (ps comm), not argv, and must not use grep -q under pipefail
  assert.match(quick, /ps -axo comm=/);
  assert.doesNotMatch(quick, /moss_running\(\)[^\n]*grep -q/);
  assert.doesNotMatch(quick, /\b(pkill|killall)\b/, "must never force-kill running MOSS or voice helper");
  assert.doesNotMatch(quick, /\bkill\s+-?\d*\s*\$/, "must never force-kill running MOSS or voice helper");
});

test("Vite dev server pins 127.0.0.1:5177 with React Fast Refresh while keeping packaged voice-private entry", () => {
  const viteConfig = readRepoText("frontend/vite.config.mjs");
  assert.match(viteConfig, /@vitejs\/plugin-react/);
  assert.match(viteConfig, /host:\s*"127\.0\.0\.1"/);
  assert.match(viteConfig, /port:\s*5177/);
  assert.match(viteConfig, /strictPort:\s*true/);
  assert.match(viteConfig, /voice-private\.html/);

  const tauriConf = JSON.parse(readRepoText("apps/desktop/src-tauri/tauri.conf.json"));
  assert.equal(Object.hasOwn(tauriConf.build ?? {}, "devUrl"), false, "voice-private must stay on packaged tauri.localhost");
});

test("MOSS_DEV_UI_ORIGIN is restricted to debug builds and loopback Vite ports only", () => {
  const libRs = readRepoText("apps/desktop/src-tauri/src/lib.rs");
  assert.match(libRs, /fn resolve_dev_ui_origin\(\)\s*->\s*Option<String>[\s\S]*?#\[cfg\(not\(debug_assertions\)\)\]\s*\{\s*None\s*\}/);
  assert.match(libRs, /"http:\/\/127\.0\.0\.1:5177"\s*\|\s*"http:\/\/127\.0\.0\.1:5173"/);
  assert.match(libRs, /is_trusted_main_with_dev/);
});

test("Magpie-style cloud parallel release workflow and slimmed sidecar script stay pure ASCII and deduplicate builds", () => {
  for (const script of ["scripts/release-tag.ps1", "scripts/build-nv18-sidecar-safe.ps1"]) {
    const bytes = readRepo(script);
    const badIndex = bytes.findIndex((byte) => byte > 0x7f);
    assert.equal(badIndex, -1, `${script} contains non-ASCII byte at offset ${badIndex}`);
  }

  const sidecar = readRepoText("scripts/build-nv18-sidecar-safe.ps1");
  assert.match(sidecar, /\[switch\]\$SkipFrontendBuild/);
  assert.match(sidecar, /\[switch\]\$IncludeFullNativeEvidence/);
  assert.match(sidecar, /\[string\]\$VersionOverride/);
  assert.match(sidecar, /\[string\]\$EmitGplSourceZip/);
  assert.match(sidecar, /beforeBuildCommand\s*=\s*''/);

  const releaseYml = readRepoText(".github/workflows/release.yml");
  assert.match(releaseYml, /name:\s*release-frontend-dist/);
  for (const job of ["windows-lite:", "windows-offline-voice:", "macos-universal:", "android-apk:", "publish:"]) {
    assert.ok(releaseYml.includes(job), `release.yml must include parallel job ${job}`);
  }
  assert.match(releaseYml, /-Dorg\.gradle\.daemon=false/);
  assert.match(releaseYml, /MOSS_RELEASE_VERSION:/);
  assert.match(releaseYml, /MOSS_\$\{VERSION\}_windows_x64_portable\.exe/);

  const androidGradle = readRepoText("apps/desktop/src-tauri/gen/android/build.gradle.kts");
  assert.match(androidGradle, /GITHUB_ACTIONS/);
  assert.match(androidGradle, /if\s*\(!isCiBuild\)/);
});
