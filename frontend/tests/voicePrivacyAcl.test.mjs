import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";

const frontend = (file) => readFileSync(new URL(`../${file}`, import.meta.url), "utf8");
const desktop = (file) => readFileSync(new URL(`../../apps/desktop/src-tauri/${file}`, import.meta.url), "utf8");
const desktopExists = (file) => existsSync(new URL(`../../apps/desktop/src-tauri/${file}`, import.meta.url));
const frontendExists = (file) => existsSync(new URL(`../${file}`, import.meta.url));
const fnBody = (source, command) => {
  const found = source.match(new RegExp(`pub async fn ${command}\\b[\\s\\S]*?(?=\\n}\\n)`));
  assert.ok(found, command);
  return found[0];
};

// 敏感语音设置的全部权限：n26（2026-10-18）四项就地展开 + n67（2026-11-07）唤醒词与固件放进语音中心
const SENSITIVE_COMMANDS = [
  "voice_memory_list", "voice_memory_set_enabled", "voice_memory_save", "voice_memory_clear",
  "voice_speaker_manage", "voice_plugins_status", "voice_plugins_save",
  "voice_home_status", "voice_home_save", "voice_home_backup_export", "voice_home_backup_audit",
  "voice_home_backup_restore", "voice_home_token_save", "voice_home_voice_consent", "voice_home_discover",
  "voice_home_approvals", "voice_home_approval_cancel", "voice_home_control_consent", "voice_home_approval_approve",
  "voice_context_manage", "voice_context_pick_folder",
  "voice_firmware_status", "voice_firmware_import", "voice_firmware_manage",
  "voice_wakeword_status", "voice_wakeword_import_base", "voice_wakeword_manage",
];

test("n67：独立安全窗口已取消，页面、入口与打开窗口的命令都不存在", () => {
  assert.equal(frontendExists("voice-private.html"), false);
  assert.equal(frontendExists("src/voicePrivate.jsx"), false);
  assert.doesNotMatch(frontend("vite.config.mjs"), /voice-private\.html/);
  assert.doesNotMatch(frontend("src/voiceApi.js"), /open_voice_private|openPrivateSettings/);
  const lib = desktop("src/lib.rs");
  assert.doesNotMatch(lib, /fn open_voice_private|WebviewUrl::App\("voice-private\.html"/);
  assert.doesNotMatch(desktop("build.rs"), /"open_voice_private"/);
  assert.equal(JSON.parse(desktop("capabilities/default.json")).permissions.includes("allow-open-voice-private"), false);
  for (const name of ["voice-memory", "voice-plugins", "voice-home", "voice-context"]) {
    assert.equal(desktopExists(`capabilities/${name}.json`), false, `${name}.json 只授给已取消的窗口，应删除`);
  }
  assert.doesNotMatch(desktop("src/voice_builtin.rs"), /is_voice_private_asset|require_voice_private_asset/);
  // 主窗口仍导航到本进程回环后端（来源判定依赖它）
  assert.match(lib, /window\.navigate\(parsed\)/);
});

test("n67：敏感语音设置只授权给主窗口，通配 / 远程 default capability 不含任何一项", () => {
  const fallback = JSON.parse(desktop("capabilities/default.json"));
  const main = JSON.parse(desktop("capabilities/voice-private-main.json"));
  assert.deepEqual(main.windows, ["main"]);
  assert.deepEqual(main.remote?.urls, ["http://127.0.0.1:*"]);
  assert.equal(Object.hasOwn(main, "webviews"), false);
  assert.equal(main.permissions.length, SENSITIVE_COMMANDS.length);
  const native = desktop("src/voice_builtin.rs") + desktop("src/voice_context.rs");
  for (const command of SENSITIVE_COMMANDS) {
    const permission = `allow-${command.replaceAll("_", "-")}`;
    assert.ok(main.permissions.includes(permission), permission);
    assert.equal(fallback.permissions.includes(permission), false, permission);
    assert.match(desktop("build.rs"), new RegExp(`"${command}"`), command);
    assert.match(fnBody(native, command), /require_private_settings_window\(&window\)\?/, `${command} 须固定主窗口来源`);
  }
  const guard = desktop("src/voice_builtin.rs").match(/fn require_private_settings_window\b[\s\S]*?(?=\n}\n)/)[0];
  assert.match(guard, /crate::window_is_trusted_main\(window\)/);
  assert.ok(fallback.remote?.urls?.some((url) => url.includes("127.0.0.1")));
});

test("四项私密设置在主语音页“隐私与家居”就地展开", () => {
  const main = frontend("src/components/VoiceBuiltinCard.jsx");
  for (const card of ["<VoiceMemoryCard", "<VoicePluginsCard", "<VoiceSpeakerCard", "<VoiceHomeCard", "<VoiceHomeApprovalCard"]) {
    assert.match(main, new RegExp(card), card);
  }
  assert.match(frontend("src/pages/VoicePage.jsx"), /<VoiceTabPanel tab="privacy"/);
  assert.doesNotMatch(frontend("src/pages/VoicePage.jsx").split('<VoiceTabPanel tab="features"')[1].split("</VoiceTabPanel>")[0], /PrivateSettingsEntry/);
  assert.match(frontend("src/components/VoiceMemoryCard.jsx"), /<VoiceContextCard\b/, "记忆面板收口为单一无感记忆卡");
  assert.doesNotMatch(frontend("src/pages/VoicePage.jsx"), /VoiceContextCard|contextManage|contextPickFolder/, "长期记忆不挂到整页顶层");
});

test("n67：唤醒词卡挂在“唤醒词与音量”，固件卡挂在“设备与工具”，推送前仍逐台确认", () => {
  const panel = frontend("src/components/VoiceWakeVolumePanel.jsx");
  assert.match(panel, /<VoiceWakeWordCard \/>/);
  assert.match(panel, /active && <VoiceWakeWordCard/, "只在标签打开时挂载，避免后台轮询");
  assert.match(panel, /deviceRequest\(row\.device_id, "set_volume"/);
  const page = frontend("src/pages/VoicePage.jsx");
  const devices = page.split('<VoiceTabPanel tab="devices"')[1].split("</VoiceTabPanel>")[0];
  assert.match(devices, /<VoiceFirmwareCard \/>/);
  assert.match(devices, /lazy>/, "固件卡收起时不挂载");
  const wake = frontend("src/components/VoiceWakeWordCard.jsx");
  assert.match(wake, /showConfirm\(/, "唤醒词推送前逐台确认");
  assert.match(wake, /confirmed: true/);
  const firmware = frontend("src/components/VoiceFirmwareCard.jsx");
  assert.match(firmware, /showConfirm\(/, "固件推送前逐台确认");
  const native = desktop("src/voice_builtin.rs");
  const firmwareImport = fnBody(native, "voice_firmware_import");
  assert.match(firmwareImport, /blocking_pick_file\(\)/, "固件文件只能由本机文件框选择");
  assert.match(firmwareImport, /add_filter\("固件",\s*&\["bin"\]\)/, "只接受 .bin 固件文件");
  assert.doesNotMatch(fnBody(native, "voice_firmware_manage"), /std::fs::remove_file|std::fs::write/, "写入与删除只由内部按 SHA-256 处理");
  assert.match(fnBody(native, "voice_wakeword_import_base"), /blocking_pick_file\(\)/, "资源包只能由本机文件框选择");
});

test("n67：全局审批卡的家居控制改为跳到语音中心家居审批，不再打开窗口", () => {
  const center = frontend("src/components/ApprovalCenter.jsx");
  assert.match(center, /requestVoiceFocus\(HOME_APPROVALS\)/);
  assert.doesNotMatch(center, /openPrivateSettings/);
  assert.match(frontend("src/components/VoiceBuiltinCard.jsx"), /peekVoiceFocus\(\) === HOME_APPROVALS/);
  assert.match(frontend("src/pages/VoicePage.jsx"), /peekVoiceFocus\(\) === HOME_APPROVALS \? "privacy"/);
});

test("NV15 historical file picker is native read-only", () => {
  const native = desktop("src/voice_builtin.rs");
  assert.match(desktop("src/lib.rs"), /voice_builtin::voice_home_backup_audit/);
  assert.match(frontend("src/voiceApi.js"), /homeBackupAudit: \(\) => invokeVoice\("voice_home_backup_audit"\)/);
  assert.match(frontend("src/components/VoiceHomeCard.jsx"), /选择历史备份文件并审计/);
  const fn = fnBody(native, "voice_home_backup_audit");
  assert.match(fn, /blocking_pick_file\(\)/);
  assert.match(fn, /audit_selected_file\(&path\)/);
  assert.doesNotMatch(fn, /std::fs::write|remove_file|remove_dir|set_len/);
  assert.match(fnBody(native, "voice_speaker_manage"), /desktop_only/);
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
  const stop = fnBody(native, "voice_agent_stop");
  assert.match(stop, /require_main_window\(&window\)\?/);
  const save = native.match(/pub fn agent_save\b[\s\S]*?(?=\n {8}\}\n)/)[0];
  const normalize = native.match(/fn normalize_agent_save\b[\s\S]*?(?=\n {4}\}\n)/)[0];
  assert.doesNotMatch(save, /config\.enabled\s*=\s*false/, "不再把开关强制改回关闭");
  assert.doesNotMatch(normalize, /config\.enabled\s*=\s*false/, "不再把开关强制改回关闭");
  assert.match(save, /normalize_agent_save\(/);
  assert.match(normalize, /if\s+!config\.enabled\s*\{[\s\S]*?if\s+config\.workspace\.trim\(\)\.is_empty\(\)/);
});

test("2026-11-07 应用内更新命令只认主窗口，更新插件的前端命令不授权给页面", () => {
  const fallback = JSON.parse(desktop("capabilities/default.json"));
  for (const command of ["app_update_status", "app_update_install"]) {
    assert.match(desktop("build.rs"), new RegExp(`"${command}"`));
    assert.ok(fallback.permissions.includes(`allow-${command.replaceAll("_", "-")}`), command);
  }
  const files = ["default", "voice-private-main", "voice-agent", "voice-mcp", "voice-music", "voice-vision"];
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
