import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const frontend = (file) => readFileSync(new URL(`../${file}`, import.meta.url), "utf8");
const desktop = (file) => readFileSync(new URL(`../../apps/desktop/src-tauri/${file}`, import.meta.url), "utf8");

test("NV16 biometric enrollment stays private, origin-gated and absent from main", () => {
  const memory=JSON.parse(desktop("capabilities/voice-memory.json"));
  assert.deepEqual(memory.windows,["voice-private"]);
  assert.ok(memory.permissions.includes("allow-voice-speaker-manage"));
  assert.equal(JSON.parse(desktop("capabilities/default.json")).permissions.includes("allow-voice-speaker-manage"),false);
  const native=desktop("src/voice_builtin.rs");
  const command=native.slice(native.indexOf("pub async fn voice_speaker_manage"),native.indexOf("pub async fn voice_home_approval_approve"));
  assert.match(command,/require_voice_private_asset\(&window\)/);
  assert.match(command,/desktop_only/);
  assert.match(frontend("src/voicePrivate.jsx"),/<VoiceSpeakerCard/);
  assert.doesNotMatch(frontend("src/components/VoiceBuiltinCard.jsx"),/<VoiceSpeakerCard/);
});

test("voice plugins and private memory ACL stay packaged voice-private only", () => {
  const fallback = JSON.parse(desktop("capabilities/default.json"));
  const memory = JSON.parse(desktop("capabilities/voice-memory.json"));
  const plugins = JSON.parse(desktop("capabilities/voice-plugins.json"));

  assert.deepEqual(memory.windows, ["voice-private"]);
  assert.deepEqual(plugins.windows, ["voice-private"]);
  for (const value of [memory, plugins]) {
    assert.equal(Object.hasOwn(value, "remote"), false);
    assert.equal(Object.hasOwn(value, "webviews"), false);
    for (const permission of value.permissions) {
      assert.equal(fallback.permissions.includes(permission), false, permission);
    }
  }
  assert.ok(fallback.permissions.includes("allow-open-voice-private"));
  assert.ok(fallback.remote?.urls?.some((url) => url.includes("127.0.0.1")));
});

test("main webview no longer invokes sensitive read functions; packaged page owns UI", () => {
  const main = frontend("src/components/VoiceBuiltinCard.jsx");
  const privateEntry = frontend("src/voicePrivate.jsx");
  assert.doesNotMatch(main, /<Voice(Plugins|Memory)Card\b/);
  assert.match(main, /voice-private-settings-open/);
  assert.match(main, /voiceApi\.openPrivateSettings\(/);
  assert.match(privateEntry, /<VoicePluginsCard\b/);
  assert.match(privateEntry, /<VoiceMemoryCard\b/);
  assert.match(frontend("voice-private.html"), /src\/voicePrivate\.jsx/);
  assert.match(frontend("vite.config.mjs"), /voice-private\.html/);
});

test("NV15 HA token, allowlist and read-only discovery require packaged private window", () => {
  const home = JSON.parse(desktop("capabilities/voice-home.json"));
  const fallback = JSON.parse(desktop("capabilities/default.json"));
  const native = desktop("src/voice_builtin.rs");
  const main = frontend("src/components/VoiceBuiltinCard.jsx");
  const privatePage = frontend("src/voicePrivate.jsx");
  assert.deepEqual(home.windows, ["voice-private"]);
  assert.equal(Object.hasOwn(home, "remote"), false);
  assert.equal(Object.hasOwn(home, "webviews"), false);
  assert.doesNotMatch(main, /<VoiceHomeCard\b/);
  assert.match(privatePage, /<VoiceHomeCard\b/);
  for (const command of ["voice_home_status", "voice_home_save", "voice_home_token_save", "voice_home_discover"]) {
    assert.equal(fallback.permissions.includes(`allow-${command.replaceAll("_", "-")}`), false, command);
    const found = native.match(new RegExp(`pub async fn ${command}\\b[\\s\\S]*?(?=\\n}\\n)`));
    assert.ok(found, command);
    assert.match(found[0], /require_voice_private_asset\(&window\)\?/);
  }
});

test("NV15 historical file picker is native read-only and private-window-only", () => {
  const name = "voice_home_backup_audit";
  const home = JSON.parse(desktop("capabilities/voice-home.json"));
  const fallback = JSON.parse(desktop("capabilities/default.json"));
  const native = desktop("src/voice_builtin.rs");
  assert.ok(home.permissions.includes("allow-voice-home-backup-audit"));
  assert.ok(!fallback.permissions.includes("allow-voice-home-backup-audit"));
  assert.match(desktop("build.rs"), /"voice_home_backup_audit"/);
  assert.match(desktop("src/lib.rs"), /voice_builtin::voice_home_backup_audit/);
  assert.match(frontend("src/voiceApi.js"), /homeBackupAudit: \(\) => invokeVoice\("voice_home_backup_audit"\)/);
  assert.match(frontend("src/components/VoiceHomeCard.jsx"), /选择历史备份文件并审计/);
  const fn = native.match(new RegExp(`pub async fn ${name}\\b[\\s\\S]*?(?=\\n}\\n)`));
  assert.ok(fn);
  assert.match(fn[0], /require_voice_private_asset\(&window\)\?/);
  assert.match(fn[0], /blocking_pick_file\(\)/);
  assert.match(fn[0], /audit_selected_file\(&path\)/);
  assert.doesNotMatch(fn[0], /std::fs::write|remove_file|remove_dir|set_len/);
});

test("native guard covers reads and writes without changing main loopback navigation", () => {
  const native = desktop("src/voice_builtin.rs");
  const lib = desktop("src/lib.rs");
  const manifest = desktop("build.rs");
  const guarded = [
    "voice_plugins_status", "voice_plugins_save", "voice_memory_list",
    "voice_memory_set_enabled", "voice_memory_save", "voice_memory_clear",
  ];
  for (const command of guarded) {
    const functionBody = native.match(new RegExp(`pub async fn ${command}\\b[\\s\\S]*?(?=\\n}\\n)`));
    assert.ok(functionBody, command);
    assert.match(functionBody[0], /require_voice_private_asset\(&window\)\?/);
    assert.ok(manifest.includes(`"${command}"`));
  }
  assert.match(lib, /on_navigation\(\|url\|/);
  assert.match(lib, /WebviewUrl::App\("voice-private\.html"/);
  assert.match(lib, /window\.navigate\(parsed\)/);
  assert.match(lib, /if window\.label\(\) != "main"/);
  assert.match(manifest, /"open_voice_private"/);
});

test("NV19 long-term context UI and commands live only in the packaged private window", () => {
  const context = JSON.parse(desktop("capabilities/voice-context.json"));
  const fallback = JSON.parse(desktop("capabilities/default.json"));
  assert.deepEqual(context.windows, ["voice-private"]);
  assert.equal(Object.hasOwn(context, "remote"), false);
  assert.equal(Object.hasOwn(context, "webviews"), false);
  for (const permission of context.permissions) assert.equal(fallback.permissions.includes(permission), false, permission);
  assert.match(frontend("src/voicePrivate.jsx"), /<VoiceContextCard\b/);
  for (const main of ["src/pages/VoicePage.jsx", "src/components/VoiceBuiltinCard.jsx"]) {
    assert.doesNotMatch(frontend(main), /VoiceContextCard|contextManage|contextPickFolder/, main);
  }
  const native = desktop("src/voice_context.rs");
  for (const command of ["voice_context_manage", "voice_context_pick_folder"]) {
    const body = native.slice(native.indexOf(`pub async fn ${command}`));
    assert.match(body.slice(0, body.indexOf("\n}\n")), /require_voice_private_asset\(&window\)\?/, command);
  }
});
