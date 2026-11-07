import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const frontend = (file) => readFileSync(new URL(`../${file}`, import.meta.url), "utf8");
const desktop = (file) => readFileSync(new URL(`../../apps/desktop/src-tauri/${file}`, import.meta.url), "utf8");

test("NV16 声纹登记仍按来源门禁，n26 起主窗口就地展开同样受限", () => {
  const memory=JSON.parse(desktop("capabilities/voice-memory.json"));
  assert.deepEqual(memory.windows,["voice-private"]);
  assert.ok(memory.permissions.includes("allow-voice-speaker-manage"));
  assert.equal(JSON.parse(desktop("capabilities/default.json")).permissions.includes("allow-voice-speaker-manage"),false);
  const native=desktop("src/voice_builtin.rs");
  const command=native.slice(native.indexOf("pub async fn voice_speaker_manage"),native.indexOf("pub async fn voice_home_approval_approve"));
  assert.match(command,/require_private_settings_window\(&window\)/);
  assert.match(command,/desktop_only/);
  assert.match(frontend("src/voicePrivate.jsx"),/<VoiceSpeakerCard/);
  assert.match(frontend("src/components/VoiceBuiltinCard.jsx"),/<VoiceSpeakerCard/);
  assert.match(desktop("capabilities/voice-private-main.json"),/allow-voice-speaker-manage/);
});

test("n26：私密窗口能力保持最小影子集，主窗口就地展开的授权单独成文件", () => {
  const fallback = JSON.parse(desktop("capabilities/default.json"));
  const memory = JSON.parse(desktop("capabilities/voice-memory.json"));
  const plugins = JSON.parse(desktop("capabilities/voice-plugins.json"));

  assert.deepEqual(memory.windows, ["voice-private"]);
  assert.deepEqual(plugins.windows, ["voice-private"]);
  const mainCapability = JSON.parse(desktop("capabilities/voice-private-main.json"));
  assert.deepEqual(mainCapability.windows, ["main"]);
  assert.deepEqual(mainCapability.remote?.urls, ["http://127.0.0.1:*"]);
  assert.equal(Object.hasOwn(mainCapability, "webviews"), false);
  // 固件与 n61 设备唤醒词都会写设备闪存，只给打包私密窗口
  const flashWriting = (permission) => permission.startsWith("allow-voice-firmware") || permission.startsWith("allow-voice-wakeword");
  for (const permission of [...memory.permissions, ...plugins.permissions]) {
    if (!flashWriting(permission)) {
      assert.ok(mainCapability.permissions.includes(permission), permission);
    }
  }
  assert.equal(mainCapability.permissions.some(flashWriting), false, "固件与唤醒词仍需打包私密窗口");
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

test("n26：四项私密设置在主语音页就地展开，ACL 只多给可信主窗口", () => {
  const main = frontend("src/components/VoiceBuiltinCard.jsx");
  const privateEntry = frontend("src/voicePrivate.jsx");
  for (const card of ["<VoiceMemoryCard", "<VoicePluginsCard"]) assert.match(main, new RegExp(card), card);
  assert.match(desktop("capabilities/voice-private-main.json"), /"windows": \[\s*"main"\s*\]/);
  assert.match(main, /voice-private-settings-open/);
  assert.match(frontend("src/pages/VoicePage.jsx"), /<VoiceTabPanel tab="privacy"/);
  assert.doesNotMatch(frontend("src/pages/VoicePage.jsx").split('<VoiceTabPanel tab="features"')[1].split('</VoiceTabPanel>')[0], /PrivateSettingsEntry/);
  for (const target of ["memory", "plugins", "speaker", "home", "approvals", "firmware"]) {
    assert.ok(desktop("src/lib.rs").includes(`Some("${target}")`), `native target allowlist: ${target}`);
  }
  assert.match(desktop("src/lib.rs"), /private_section_script/);
  assert.match(desktop("src/lib.rs"), /on_page_load/);
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
  assert.match(main, /<VoiceHomeCard\b/, "家居设置在主语音页就地展开");
  assert.match(desktop("capabilities/voice-private-main.json"), /allow-voice-home-token-save/);
  assert.match(privatePage, /<VoiceHomeCard\b/);
  for (const command of ["voice_home_status", "voice_home_save", "voice_home_token_save", "voice_home_discover"]) {
    assert.equal(fallback.permissions.includes(`allow-${command.replaceAll("_", "-")}`), false, command);
    const found = native.match(new RegExp(`pub async fn ${command}\\b[\\s\\S]*?(?=\\n}\\n)`));
    assert.ok(found, command);
    assert.match(found[0], /require_private_settings_window\(&window\)\?/);
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
  assert.match(fn[0], /require_private_settings_window\(&window\)\?/);
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
    assert.match(functionBody[0], /require_private_settings_window\(&window\)\?/);
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
  assert.match(frontend("src/voicePrivate.jsx"), /<VoiceMemoryCard\b/);
  assert.match(frontend("src/components/VoiceMemoryCard.jsx"), /<VoiceContextCard\b/, "记忆面板收口为单一无感记忆卡");
  assert.match(frontend("src/components/VoiceBuiltinCard.jsx"), /<VoiceMemoryCard\b/, "主语音页就地展开合一记忆卡");
  assert.doesNotMatch(frontend("src/pages/VoicePage.jsx"), /VoiceContextCard|contextManage|contextPickFolder/, "长期记忆不挂到整页顶层");
  const native = desktop("src/voice_context.rs");
  for (const command of ["voice_context_manage", "voice_context_pick_folder"]) {
    const body = native.slice(native.indexOf(`pub async fn ${command}`));
    assert.match(body.slice(0, body.indexOf("\n}\n")), /require_private_settings_window\(&window\)\?/, command);
  }
});

test("NV17 firmware import, push and cancel stay private-window-only with native file picking", () => {
  const memory = JSON.parse(desktop("capabilities/voice-memory.json"));
  const fallback = JSON.parse(desktop("capabilities/default.json"));
  const native = desktop("src/voice_builtin.rs");
  assert.deepEqual(memory.windows, ["voice-private"]);
  assert.equal(Object.hasOwn(memory, "remote"), false);
  for (const command of ["voice_firmware_status", "voice_firmware_import", "voice_firmware_manage"]) {
    const permission = `allow-${command.replaceAll("_", "-")}`;
    assert.ok(memory.permissions.includes(permission), command);
    assert.equal(fallback.permissions.includes(permission), false, command);
    const body = native.match(new RegExp(`pub async fn ${command}\\b[\\s\\S]*?(?=\\n}\\n)`));
    assert.ok(body, command);
    assert.match(body[0], /require_voice_private_asset\(&window\)\?/);
  }
  assert.match(desktop("build.rs"), /"voice_firmware_import"/);
  assert.match(desktop("build.rs"), /"voice_firmware_manage"/);
  assert.match(frontend("src/voicePrivate.jsx"), /<VoiceFirmwareCard\b/);
  assert.doesNotMatch(frontend("src/pages/VoicePage.jsx"), /VoiceFirmwareCard/);
  const importBody = native.match(/pub async fn voice_firmware_import\b[\s\S]*?(?=\n}\n)/)[0];
  assert.match(importBody, /blocking_pick_file\(\)/, "固件文件只能由本机文件框选择");
  assert.match(importBody, /add_filter\("固件",&\["bin"\]\)/);
  const manageBody = native.match(/pub async fn voice_firmware_manage\b[\s\S]*?(?=\n}\n)/)[0];
  assert.doesNotMatch(manageBody, /std::fs::remove_file|std::fs::write/, "写入与删除只由内部按 SHA-256 处理");
});

test("n61 wake word status, base import and push stay private-window-only with native file picking", () => {
  const memory = JSON.parse(desktop("capabilities/voice-memory.json"));
  const fallback = JSON.parse(desktop("capabilities/default.json"));
  const main = JSON.parse(desktop("capabilities/voice-private-main.json"));
  const native = desktop("src/voice_builtin.rs");
  for (const command of ["voice_wakeword_status", "voice_wakeword_import_base", "voice_wakeword_manage"]) {
    const permission = `allow-${command.replaceAll("_", "-")}`;
    assert.ok(memory.permissions.includes(permission), command);
    assert.equal(fallback.permissions.includes(permission), false, command);
    assert.equal(main.permissions.includes(permission), false, command);
    assert.match(desktop("build.rs"), new RegExp(`"${command}"`));
    const body = native.match(new RegExp(`pub async fn ${command}\\b[\\s\\S]*?(?=\\n}\\n)`));
    assert.ok(body, command);
    assert.match(body[0], /require_voice_private_asset\(&window\)\?/);
  }
  const importBody = native.match(/pub async fn voice_wakeword_import_base\b[\s\S]*?(?=\n}\n)/)[0];
  assert.match(importBody, /blocking_pick_file\(\)/, "资源包只能由本机文件框选择");
  assert.match(frontend("src/voicePrivate.jsx"), /<VoiceWakeWordCard\b/);
  assert.doesNotMatch(frontend("src/pages/VoicePage.jsx"), /VoiceWakeWordCard/);
  assert.doesNotMatch(frontend("src/components/VoiceBuiltinCard.jsx"), /VoiceWakeWordCard|wakewordManage/, "主窗口只放入口，不能直接推送");
  const card = frontend("src/components/VoiceWakeWordCard.jsx");
  assert.match(card, /showConfirm\(/, "推送前逐台确认");
  assert.match(card, /confirmed: true/);
});

test("NV13 agent stop is main-window-only and saving keeps the enable switch", () => {
  const agent = JSON.parse(desktop("capabilities/voice-agent.json"));
  const fallback = JSON.parse(desktop("capabilities/default.json"));
  const native = desktop("src/voice_builtin.rs");
  assert.deepEqual(agent.windows, ["main"]);
  assert.ok(agent.permissions.includes("allow-voice-agent-stop"));
  assert.equal(fallback.permissions.includes("allow-voice-agent-stop"), false);
  assert.match(desktop("build.rs"), /"voice_agent_stop"/);
  assert.match(frontend("src/voiceApi.js"), /agentStop: \(runId\) => invokeVoice\("voice_agent_stop", \{ runId \}\)/);
  const stop = native.match(/pub async fn voice_agent_stop\b[\s\S]*?(?=\n}\n)/)[0];
  assert.match(stop, /require_main_window\(&window\)\?/);
  assert.doesNotMatch(stop, /require_voice_private_asset/);
  const save = native.match(/pub fn agent_save\b[\s\S]*?(?=\n        \}\n)/)[0];
  const normalize = native.match(/fn normalize_agent_save\b[\s\S]*?(?=\n    \}\n)/)[0];
  assert.doesNotMatch(save, /config\.enabled\s*=\s*false/, "不再把开关强制改回关闭");
  assert.doesNotMatch(normalize, /config\.enabled\s*=\s*false/, "不再把开关强制改回关闭");
  assert.match(save, /normalize_agent_save\(/);
  assert.match(normalize, /if\s+!config\.enabled\s*\{[\s\S]*?if\s+config\.workspace\.trim\(\)\.is_empty\(\)/);
});

test("2026-11-07 唤醒词单独入口：主窗口只打开私密 wakeword 任务，音量走主窗口设备请求", () => {
  assert.ok(desktop("src/lib.rs").includes('Some("wakeword") => "wakeword"'), "原生分区白名单含 wakeword");
  const panel = frontend("src/components/VoiceWakeVolumePanel.jsx");
  assert.match(panel, /openPrivateSettings\("wakeword"\)/);
  assert.doesNotMatch(panel, /wakewordManage|wakewordStatus|VoiceWakeWordCard/, "主窗口不能读写唤醒词");
  assert.match(panel, /deviceRequest\(row\.device_id, "set_volume"/);
  const privatePage = frontend("src/voicePrivate.jsx");
  assert.match(privatePage, /section !== "wakeword"\}><VoiceWakeWordCard \/>/);
  assert.match(privatePage, /section !== "firmware"\}><VoiceFirmwareCard \/>/);
});

test("2026-11-07 应用内更新命令只认主窗口，更新插件的前端命令不授权给页面", () => {
  const fallback = JSON.parse(desktop("capabilities/default.json"));
  for (const command of ["app_update_status", "app_update_install"]) {
    assert.match(desktop("build.rs"), new RegExp(`"${command}"`));
    assert.ok(fallback.permissions.includes(`allow-${command.replaceAll("_", "-")}`), command);
  }
  const files = ["default", "voice-private-main", "voice-memory", "voice-plugins", "voice-home", "voice-agent", "voice-context", "voice-mcp", "voice-music", "voice-vision"];
  for (const name of files) {
    assert.doesNotMatch(desktop(`capabilities/${name}.json`), /updater:/, `${name} 不能授权 updater 插件命令`);
  }
  const native = desktop("src/app_update.rs");
  for (const command of ["app_update_status", "app_update_install"]) {
    const body = native.match(new RegExp(`pub async fn ${command}\\b[\\s\\S]*?require_main\\(&window\\)\\?`));
    assert.ok(body, `${command} 先校验主窗口`);
  }
  assert.match(native, /device_work_in_progress\(\)/, "加工中拒绝更新");
  assert.match(native, /on_before_exit\(move \|\| crate::shutdown_desktop_services/, "Windows 安装器结束进程前走急停收尾");
});
