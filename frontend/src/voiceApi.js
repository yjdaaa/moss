/**
 * 语音中心的 Tauri 命令封装（技术书 §9.4.6）。命令只存在于 MOSS 桌面端；
 * 错误统一抛出带 `code` 的 Error，页面用 voiceErrorText 显示。
 */
import { hasDesktopSession, isRemoteMode } from "./authSession.js";
import { voiceCenterAvailable } from "./voiceState.js";

function tauriInvoke() {
  return typeof window !== "undefined" ? window.__TAURI__?.core?.invoke : undefined;
}

export function isVoiceCenterAvailable() {
  return voiceCenterAvailable({
    hasInvoke: Boolean(tauriInvoke()),
    desktopSession: hasDesktopSession(),
    remote: isRemoteMode(),
  });
}

export async function invokeVoice(command, args = {}) {
  const invoke = tauriInvoke();
  if (!invoke) {
    const error = new Error("语音中心仅在 MOSS 桌面端可用");
    error.code = "voice_desktop_only";
    throw error;
  }
  try {
    return await invoke(command, args);
  } catch (raw) {
    const error = new Error(typeof raw === "string" ? raw : raw?.message || "操作失败");
    error.code = raw?.code || "";
    throw error;
  }
}

export const voiceApi = {
  speakerManage: (input) => invokeVoice("voice_speaker_manage", { input }),
  // NV19 长期记忆（仅私有窗口）：按 action 分发；选资料文件夹后在本机直接建立索引，路径不回传
  contextManage: (input) => invokeVoice("voice_context_manage", { input }),
  contextPickFolder: () => invokeVoice("voice_context_pick_folder"),
  firmwareStatus: () => invokeVoice("voice_firmware_status"),
  mcpStatus: () => invokeVoice("voice_mcp_status"),
  mcpSave: (input) => invokeVoice("voice_mcp_save", { input }),
  mcpDelete: (name, revision, confirmed) => invokeVoice("voice_mcp_delete", { name, revision, confirmed }),
  mcpReconnect: (name, confirmed) => invokeVoice("voice_mcp_reconnect", { name, confirmed }),
  status: () => invokeVoice("voice_status"),
  saveSettings: (settings) => invokeVoice("voice_save_settings", { settings }),
  modeChanges: (mode) => invokeVoice("voice_mode_changes", { mode }),
  setMode: (mode) => invokeVoice("voice_set_mode", { mode }),
  serverStart: () => invokeVoice("voice_server_start"),
  serverStop: () => invokeVoice("voice_server_stop"),
  serverRestart: () => invokeVoice("voice_server_restart"),
  serverLog: (tail = 200) => invokeVoice("voice_server_log", { tail }),
  modelsTest: (section, input) => invokeVoice("voice_models_test", { section, input }),
  modelsSave: (input) => invokeVoice("voice_models_save", { input }),
  musicPickDirectory: () => invokeVoice("voice_music_pick_directory"),
  musicStatus: () => invokeVoice("voice_music_status"),
  musicSave: (input) => invokeVoice("voice_music_save", { input }),
  homeStatus: () => invokeVoice("voice_home_status"),
  homeSave: (input) => invokeVoice("voice_home_save", { input }),
  homeBackupExport: () => invokeVoice("voice_home_backup_export"),
  homeBackupAudit: () => invokeVoice("voice_home_backup_audit"),
  homeBackupRestore: (input) => invokeVoice("voice_home_backup_restore", { input }),
  homeTokenSave: (input) => invokeVoice("voice_home_token_save", { input }),
  homeVoiceConsent: (input) => invokeVoice("voice_home_voice_consent", { input }),
  homeApprovals: () => invokeVoice("voice_home_approvals"),
  homeApprovalCancel: (input) => invokeVoice("voice_home_approval_cancel", { input }),
  homeControlConsent: (input) => invokeVoice("voice_home_control_consent", { input }),
  homeApprovalApprove: (input) => invokeVoice("voice_home_approval_approve", { input }),
  homeDiscover: () => invokeVoice("voice_home_discover"),
  agentStatus: () => invokeVoice("voice_agent_status"),
  agentSave: (input) => invokeVoice("voice_agent_save", { input }),
  visionStatus: () => invokeVoice("voice_vision_status"),
  visionSave: (input) => invokeVoice("voice_vision_save", { input }),
  visionTest: () => invokeVoice("voice_vision_test"),
  openPrivateSettings: () => invokeVoice("open_voice_private"),
  pluginsStatus: () => invokeVoice("voice_plugins_status"),
  pluginsSave: (input) => invokeVoice("voice_plugins_save", { input }),
  memoryList: () => invokeVoice("voice_memory_list"),
  memorySetEnabled: (enabled) => invokeVoice("voice_memory_set_enabled", { enabled }),
  memorySave: (deviceId, expectedRevision, summary) => invokeVoice("voice_memory_save", { deviceId, expectedRevision, summary }),
  memoryClear: (deviceId, expectedRevision) => invokeVoice("voice_memory_clear", { deviceId, expectedRevision }),
  // 内置语音服务（内置语音技术书 NV1）
  builtinStatus: () => invokeVoice("voice_builtin_status"),
  builtinStart: () => invokeVoice("voice_builtin_start"),
  builtinStop: () => invokeVoice("voice_builtin_stop"),
  deviceSetStatus: (deviceId, status) => invokeVoice("voice_device_set_status", { deviceId, status }),
  deviceRename: (deviceId, name) => invokeVoice("voice_device_rename", { deviceId, name }),
  // 内置语音服务的模型配置（NV5）：密钥留空表示沿用已保存的密钥；clear 中的分区删除设置与密钥
  builtinModelsTest: (section, input) => invokeVoice("voice_builtin_models_test", { section, input }),
  builtinModelsSave: (input, clear = []) => invokeVoice("voice_builtin_models_save", { input, clear }),
  // 本地模型（NV6）：下载进度在 builtinStatus().local.models 中
  localSave: (settings) => invokeVoice("voice_local_save", { settings }),
  localDownload: (id) => invokeVoice("voice_local_model_download", { id }),
  localCancel: (id) => invokeVoice("voice_local_model_cancel", { id }),
  localDelete: (id) => invokeVoice("voice_local_model_delete", { id }),
};
