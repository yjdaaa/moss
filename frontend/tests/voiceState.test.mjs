// Voice settings and diagnostics are exercised with synthetic values only.
import test from "node:test";
import assert from "node:assert/strict";

test("NV8a 热词规范化、Unicode 长度、数量和字节上限", () => {
  const form = localSettingsForm({ custom_hotwords: ["椴木", "亚克力"] });
  assert.equal(form.custom_hotwords, "椴木\n亚克力");
  assert.equal(form.read_only_shortcuts, true);
  assert.equal(form.asr_beam_search, true);
  const input = { ...form, custom_hotwords: "  椴木  \r\n\r\n亚克力\n椴木", read_only_shortcuts: false, asr_beam_search: false };
  assert.equal(localSettingsError(input), "");
  const payload = localSettingsPayload(input);
  assert.deepEqual(payload.custom_hotwords, ["椴木", "亚克力"]);
  assert.equal(payload.read_only_shortcuts, false);
  assert.equal(payload.asr_beam_search, false);
  assert.equal(localSettingsError({ ...form, custom_hotwords: "木".repeat(32) }), "");
  assert.match(localSettingsError({ ...form, custom_hotwords: "木".repeat(33) }), /32/);
  assert.match(localSettingsError({ ...form, custom_hotwords: "bad\tword" }), /控制字符/);
  assert.match(localSettingsError({ ...form, custom_hotwords: Array.from({ length: 65 }, (_, i) => `材料${i}`).join("\n") }), /64/);
  assert.match(localSettingsError({ ...form, custom_hotwords: Array.from({ length: 64 }, (_, i) => `${"椴".repeat(30)}${i}`).join("\n") }), /4096/);
  assert.equal(localSettingsError({ ...form, custom_hotwords: Array(65).fill("椴木").join("\n") }), "");
});

test("NV8a 复制诊断仅保留允许字段，不复制对话、音频、凭据和地址；n68 带会话号与设备编号", () => {
  const sensitive = "never-copy-this-value";
  const item = {
    session_id: "s-1", device_id: "aa:bb:cc:11:22:33", at_ms: 1_790_000_000_123,
    turn: 3, outcome: "interrupted", models: ["fake-asr"],
    transcript: sensitive, audio: sensitive, authorization: sensitive, url: sensitive, ip: sensitive,
    stages: { recording_start: 0, barge_in: 430, first_audio_frame: 750, unknown: sensitive, final_asr: Infinity, new_stage: 12 },
    tools: Array.from({ length: 70 }, () => ({ name: "fake_status", ms: 15, arguments: sensitive, result: sensitive })),
  };
  const report = voiceTimingReport(Array(60).fill(item));
  assert.equal(report.length, 50, "后端最多保留 50 轮，复制时全部带上");
  assert.equal(report[0].tools.length, 50);
  assert.deepEqual(report[0].stages, { barge_in: 430, recording_start: 0, first_audio_frame: 750, new_stage: 12 }, "未登记的数字阶段原样保留，非数字丢弃");
  assert.deepEqual(report[0].tools[0], { name: "fake_status", ms: 15 });
  assert.equal(report[0].session_id, "s-1");
  assert.equal(report[0].device_id, "aa:bb:cc:11:22:33");
  assert.equal(report[0].at_ms, 1_790_000_000_123);
  assert.equal(JSON.stringify(report).includes(sensitive), false);
  assert.deepEqual(voiceTimingReport(null), []);
});
import {
  approvalHistoryStatus,
  canSaveSection,
  deviceView,
  builtinDeviceControlsView,
  parseAsrCorrectionLine,
  emptyForm,
  finalUrl,
  isSectionBlank,
  maskLogLine,
  modeConfirmText,
  sectionFingerprint,
  sectionInputError,
  sectionPayload,
  serverStateView,
  voiceCenterAvailable,
  voiceErrorText,
  builtinDeviceView,
  allowDeviceConfirmText,
  builtinModeView,
  builtinModelForm,
  builtinKeyPlaceholder,
  DOUBAO_DEFAULTS,
  switchKind,
  formatBytes,
  localModelGroups,
  localModelView,
  layaSetupView,
  localJevRuntimeView,
  localModelDeleteText,
  selectLocalModel,
  selectedVoices,
  voiceTimingReport,
  localSettingsForm,
  localSettingsError,
  localSettingsPayload,
  MAX_CUSTOM_PERSONAS,
  newCustomPersonaId,
  parseCustomPersonas,
  personaOptions,
  endpointLabel,
  endpointView,
  identityView,
  toolSwitchPayload,
  toolStatText,
  TOOL_EDIT_PROMPTS,
  toolEditPrompt,
  extraEndpointError,
  SETTINGS_GROUPS,
  LOCAL_MODEL_KEYS,
  EXPERIENCE_KEYS,
  formDirty,
  settingsGroupDirty,
  settingsGroupPayload,
  settingsSaveEffect,
  experienceSettingsError,
  localModelSettingsError,
  mergeGroup,
  localGroupPatch,
  savedNoteText,
  dirtyTabs,
  builtinModelSummary,
  builtinJevForm,
  builtinJevSavePayload,
  externalModelSummary,
} from "../src/voiceState.js";

test("语音中心只在桌面端（有 invoke、有桌面会话、非手机远程模式）可用（A-69）", () => {
  assert.equal(voiceCenterAvailable({ hasInvoke: true, desktopSession: true, remote: false }), true);
  assert.equal(voiceCenterAvailable({ hasInvoke: false, desktopSession: true, remote: false }), false);
  assert.equal(voiceCenterAvailable({ hasInvoke: true, desktopSession: false, remote: false }), false);
  assert.equal(voiceCenterAvailable({ hasInvoke: true, desktopSession: true, remote: true }), false);
});

test("最终地址与桌面端规则一致", () => {
  assert.equal(finalUrl("llm", " https://h/v1/ "), "https://h/v1");
  assert.equal(finalUrl("llm", "https://h/v1/chat/completions"), "https://h/v1");
  assert.equal(finalUrl("asr", "https://h/v1"), "https://h/v1/audio/transcriptions");
  assert.equal(finalUrl("asr", "https://h/v1/audio/transcriptions/"), "https://h/v1/audio/transcriptions");
  assert.equal(finalUrl("tts", "https://h/v1"), "https://h/v1/audio/speech");
  assert.equal(finalUrl("tts", ""), "");
});

test("必填检查与留空分区", () => {
  assert.equal(isSectionBlank(emptyForm("llm")), true);
  assert.equal(sectionInputError("llm", { url: "", model: "m" }), "请填写地址");
  assert.equal(sectionInputError("llm", { url: "ftp://h", model: "m" }), "地址必须以 http:// 或 https:// 开头");
  assert.equal(sectionInputError("llm", { url: "https://<host>/v1", model: "m" }), "地址包含空白或占位符");
  assert.equal(sectionInputError("llm", { url: "https://h/v1", model: "" }), "请填写模型名");
  assert.equal(sectionInputError("tts", { url: "https://h", model: "tts-1", voice: "", speed: "1" }), "请填写音色");
  assert.equal(sectionInputError("tts", { url: "https://h", model: "tts-1", voice: "alloy", speed: "9" }), "语速须在 0.25 ~ 4.0 之间");
  assert.equal(sectionInputError("tts", { url: "https://h", model: "tts-1", voice: "alloy", speed: "1.2" }), "");
  assert.deepEqual(sectionPayload("tts", { url: " https://h ", api_key: " k ", model: "m", voice: "v", speed: "1.5" }), {
    url: "https://h",
    api_key: "k",
    model: "m",
    voice: "v",
    speed: 1.5,
  });
});

test("先测试后保存：测试成功且内容未改动才可保存（A-78）", () => {
  const form = { url: "https://h/v1", api_key: "sk-demo", model: "m" };
  assert.equal(canSaveSection("llm", form, null), false);
  const ok = { ok: true, fingerprint: sectionFingerprint("llm", form) };
  assert.equal(canSaveSection("llm", form, ok), true);
  assert.equal(canSaveSection("llm", { ...form, model: "m2" }, ok), false, "测试后改了模型");
  assert.equal(canSaveSection("llm", { ...form, api_key: "sk-other" }, ok), false, "测试后改了密钥");
  // 只是多写了结尾斜杠：最终地址相同，仍可保存
  assert.equal(canSaveSection("llm", { ...form, url: "https://h/v1/" }, ok), true);
  assert.equal(canSaveSection("llm", form, { ok: false, fingerprint: ok.fingerprint }), false);
  assert.equal(canSaveSection("llm", form, ok, { fullModule: true }), false, "全模块部署只测试不写");
});

test("服务端按钮：外部实例不显示停止（A-63），崩溃可重新启动（A-65）", () => {
  assert.deepEqual(
    { ...serverStateView({ state: "running", owned: true }) },
    { text: "运行中（由 MOSS 启动）", tone: "ok", state: "running", canStart: false, canStop: true, canRestart: true },
  );
  const external = serverStateView({ state: "external", owned: false });
  assert.equal(external.canStop, false);
  assert.equal(external.canStart, false);
  assert.equal(external.text, "外部运行");
  const exited = serverStateView({ state: "exited", exit_code: 3 });
  assert.equal(exited.canStart, true);
  assert.equal(exited.tone, "bad");
  assert.equal(serverStateView(null).state, "stopped");
});

test("设备状态：无法判断时显示未知，不猜（§9.4.2）", () => {
  assert.equal(deviceView("official", { state: "online", sessions: 1 }).text, "未知");
  assert.equal(deviceView("self_hosted", { state: "online", sessions: 2 }).text, "在线（2 个会话）");
  const waiting = deviceView("self_hosted", { state: "waiting", sessions: 0 });
  assert.equal(waiting.text, "等待设备");
  assert.match(waiting.hint, /OTA/);
  assert.equal(deviceView("self_hosted", { state: "unknown", reason: "健康检查超时" }).text, "未知");
  assert.match(deviceView("self_hosted", { state: "unknown", reason: "健康检查超时" }).hint, /健康检查超时/);
});

test("审批记录状态", () => {
  assert.equal(approvalHistoryStatus({ status: "pending" }).text, "待批准");
  assert.equal(approvalHistoryStatus({ status: "approved", outcome: { phase: "running" } }).text, "执行中");
  assert.equal(approvalHistoryStatus({ status: "rejected" }).text, "已拒绝");
  assert.equal(approvalHistoryStatus({ status: "expired" }).text, "已过期");
  assert.equal(approvalHistoryStatus({ status: "failed", outcome: { error_code: "approval_mismatch" } }).text, "摘要不一致");
  assert.equal(approvalHistoryStatus({ status: "executed", outcome: { phase: "completed" } }).text, "已完成");
  assert.equal(approvalHistoryStatus({ status: "executed", outcome: { phase: "cancelled", message: "用户急停" } }).text, "急停");
  assert.equal(approvalHistoryStatus({ status: "executed", outcome: { phase: "cancelled", message: "取消后续发送" } }).text, "已取消");
  assert.equal(approvalHistoryStatus({ status: "executed", outcome: { phase: "unknown" } }).text, "结果未知");
  assert.equal(approvalHistoryStatus({ status: "failed", outcome: { message: "设备未连接" } }).text, "失败");
});

test("切换确认框列出改动项；错误文字；日志兜底脱敏（A-66）", () => {
  const confirm = modeConfirmText("self_hosted", ["严格模式：开启", "mcp_endpoint 改为自建"]);
  assert.equal(confirm.title, "切换到自建服务端？");
  assert.match(confirm.message, /• 严格模式：开启/);
  assert.equal(modeConfirmText("official", []).title, "切换回官方小智？");
  assert.equal(voiceErrorText({ code: "server_not_owned", message: "" }), "正在运行的服务端不是 MOSS 启动的，请在启动它的地方关闭。");
  assert.equal(voiceErrorText({ code: "x", message: "具体原因" }), "具体原因");
  assert.equal(voiceErrorText("文字"), "文字");
  assert.equal(maskLogLine("api_key: sk-123 ok"), "api_key: *** ok");
  assert.equal(maskLogLine("Authorization: Bearer abc.def"), "Authorization: ***");
  assert.equal(maskLogLine("url ws://h/mcp/?token=abc&x=1"), "url ws://h/mcp/?token=***&x=1");
  assert.equal(maskLogLine("tokens_used=12"), "tokens_used=12");
});

test("内置服务设备：状态、显示名与可用操作（内置语音技术书 §6）", () => {
  const pending = builtinDeviceView({ device_id: "aa:bb", status: "pending" });
  assert.equal(pending.text, "待允许");
  assert.equal(pending.title, "aa:bb");
  assert.equal(pending.canAllow, true);
  assert.equal(pending.canRevoke, true);
  const allowed = builtinDeviceView({ device_id: "aa:bb", status: "allowed", name: " 工作台小智 " });
  assert.equal(allowed.title, "工作台小智");
  assert.equal(allowed.canAllow, false);
  const revoked = builtinDeviceView({ device_id: "aa:bb", status: "revoked" });
  assert.equal(revoked.canRevoke, false);
  assert.equal(revoked.canAllow, true);
  assert.equal(builtinDeviceView(null).status, "pending");
  const text = allowDeviceConfirmText({ device_id: "aa:bb", model: "lichuang-dev", firmware_version: "" });
  assert.match(text, /设备：aa:bb/);
  assert.match(text, /型号：lichuang-dev/);
  assert.doesNotMatch(text, /固件：/);
  assert.match(text, /重启设备/);
});

test("NV5: 内置服务的模式、表单预填与密钥占位（不回显密钥）", () => {
  assert.equal(builtinModeView({ mode: "chat", link_mode: "builtin" }).text, "对话");
  assert.match(builtinModeView({ mode: "chat", link_mode: "builtin" }).autostart, /随 MOSS 自动启动/);
  assert.equal(builtinModeView({ mode: "echo", link_mode: "official" }).text, "回声测试");
  assert.match(builtinModeView({ mode: "echo" }).hint, /配齐后自动切换为对话/);
  assert.deepEqual(builtinModelForm("llm", { configured: true, url: "https://h/v1", model: "m", key_set: true }), {
    url: "https://h/v1",
    api_key: "",
    model: "m",
  });
  assert.deepEqual(builtinModelForm("tts", { configured: true, url: "https://h/v1/audio/speech", model: "t", voice: "alloy", speed: 1.2 }), {
    url: "https://h/v1/audio/speech",
    api_key: "",
    model: "t",
    voice: "alloy",
    speed: "1.2",
  });
  assert.deepEqual(builtinModelForm("asr", { configured: false }), emptyForm("asr"));
  assert.match(builtinKeyPlaceholder({ key_set: true }), /留空则沿用/);
  assert.match(builtinKeyPlaceholder({ key_set: false }), /未设置/);
  assert.equal(modeConfirmText("builtin", ["a"]).title, "切换到 MOSS 内置服务？");
  assert.equal(deviceView("builtin", { state: "waiting" }).text, "见内置服务");
});

test("2026-11-07: OneJev 推理引擎准备进度、Laya 运行环境自动准备与删除提示", () => {
  const preparing = localModelView({ installed: false, progress: { state: "preparing_runtime", done: 5, total: 20 } });
  assert.deepEqual([preparing.text, preparing.percent, preparing.canCancel, preparing.canDelete], ["准备推理引擎 25%", 25, true, false]);
  const waiting = localModelView({ installed: false, progress: { state: "waiting_runtime" } });
  assert.deepEqual([waiting.text, waiting.canCancel, waiting.canDownload], ["等待推理引擎", true, false]);

  const missing = layaSetupView({ state: "idle" }, { ready: false, summary: "尚未准备 Laya 运行环境" });
  assert.deepEqual([missing.tone, missing.canStart, missing.startLabel], ["warn", true, "准备运行环境"]);
  const running = layaSetupView({ state: "running", stage: "安装 PyTorch（CUDA 12.4）", step: 4, steps: 6, detail: "Downloading torch", elapsed_secs: 75 }, { ready: false });
  assert.equal(running.text, "正在准备 Laya 运行环境：第 4/6 步 · 安装 PyTorch（CUDA 12.4）（已用时 1:15）");
  assert.deepEqual([running.canCancel, running.canStart, running.detail], [true, false, "Downloading torch"]);
  const failed = layaSetupView({ state: "failed", message: "未检测到可用的 Python", log: ["a", "b", "c", "d"] }, { ready: false });
  assert.deepEqual([failed.tone, failed.canStart, failed.detail, failed.startLabel], ["bad", true, "b\nc\nd", "重试准备环境"]);
  assert.equal(layaSetupView({ state: "done", summary: "已准备好" }, { ready: true, accelerator: "cuda" }).canStart, false);
  assert.equal(layaSetupView({ state: "done", summary: "已准备好" }, { ready: true, accelerator: "cpu" }).startLabel, "重新准备环境");
  assert.equal(layaSetupView({ state: "idle" }, { ready: true, accelerator: "mps", managed: true }), null);
  assert.equal(layaSetupView({ state: "idle" }, { ready: true, accelerator: "cpu", managed: false }), null, "用户自建环境不提示重装");

  const models = [
    { id: "onejev_1_6_0_8b", kind: "jev", label: "0.8B", installed: true },
    { id: "onejev_1_6_4b", kind: "jev", label: "4B", installed: false, progress: { state: "failed" } },
    { id: "laya_multilingual", kind: "jev", label: "Laya", installed: true },
  ];
  assert.match(localModelDeleteText(models[0], models, {}), /最后一档 OneJev/);
  assert.doesNotMatch(
    localModelDeleteText(models[0], [...models, { id: "onejev_1_6_9b", kind: "jev", progress: { state: "waiting_runtime" } }], {}),
    /最后一档/,
  );
  assert.match(localModelDeleteText(models[2], models, { laya_env: { managed: true } }), /Python 运行环境/);
  assert.doesNotMatch(localModelDeleteText(models[2], models, { laya_env: { managed: false } }), /Python 运行环境/);
  assert.equal(localModelDeleteText({ id: "kokoro", kind: "tts", label: "Kokoro" }, models, {}), "删除本地模型“Kokoro”？需要时可以重新下载。");
});

test("NV6: 本地模型状态、设置表单与校验", () => {
  assert.equal(formatBytes(643854), "629 KB");
  assert.equal(formatBytes(163002883), "155.5 MB");
  const downloading = localModelView({ installed: false, progress: { state: "downloading", done: 50, total: 200 } });
  assert.deepEqual([downloading.text, downloading.percent, downloading.canCancel, downloading.canDownload], ["下载中 25%", 25, true, false]);
  assert.equal(localModelView({ installed: true }).text, "已安装");
  assert.equal(localModelView({ installed: true }).canDelete, true);
  const failed = localModelView({ installed: false, progress: { state: "failed", message: "SHA-256 校验不一致" } });
  assert.deepEqual([failed.text, failed.detail], ["下载失败", "SHA-256 校验不一致"]);
  assert.equal(failed.canDownload, true);
  assert.equal(localModelView({ installed: false, progress: { state: "cancelled" } }).text, "已取消");
  assert.equal(localModelView({ installed: false, progress: { state: "verifying" } }).text, "校验中");
  assert.equal(localModelView(null).text, "未下载");
  const form = localSettingsForm({ asr: true, tts_speaker: 3, tts_speed: 1.2 });
  assert.deepEqual(form, {
    asr: true, tts: false, vad: false, vad_model: "silero_vad", asr_model: "sense_voice", tts_model: "kokoro",
    read_only_shortcuts: true, asr_beam_search: true, job_done_announce: true, custom_hotwords: "",
    asr_homophone_fix: true, custom_asr_corrections: "", persona_style: "workshop", persona_prompt: "", custom_personas: "[]", screen_preview_sync: true, screen_card_style: "moss_hud",
    jev_filter_enabled: false, jev_model: "typesafe/jev-1.14", jev_unfinished_hold: true, jev_fast_route: true, jev_tool_prune: true, jev_memory_filter: true,
    barge_in: false, barge_in_ms: "400",
    tts_speaker: "3", tts_speed: "1.2", mirror: "",
  });
  assert.equal(localSettingsError(form), "");
  assert.match(localSettingsError({ ...form, tts_speaker: "1.5" }), /整数/);
  assert.match(localSettingsError({ ...form, tts_speed: "3" }), /0.5/);
  assert.match(localSettingsError({ ...form, mirror: "ftp://x" }), /http/);
  assert.deepEqual(localSettingsPayload({ ...form, mirror: " https://m.test/x " }), {
    asr: true, tts: false, vad: false, vad_model: "silero_vad", asr_model: "sense_voice", tts_model: "kokoro",
    read_only_shortcuts: true, asr_beam_search: true, job_done_announce: true, custom_hotwords: [],
    asr_homophone_fix: true, custom_asr_corrections: [], persona_style: "workshop", persona_prompt: "", custom_personas: [], screen_preview_sync: true, screen_card_style: "moss_hud",
    jev_filter_enabled: false, jev_model: "typesafe/jev-1.14", jev_unfinished_hold: true, jev_fast_route: true, jev_tool_prune: true, jev_memory_filter: true,
    barge_in: false, barge_in_ms: 400,
    tts_speaker: 3, tts_speed: 1.2, mirror: "https://m.test/x",
  });
});

test("自然插话默认关闭，时长限定为 300 到 600 毫秒的整数", () => {
  const form = localSettingsForm();
  assert.equal(form.barge_in, false);
  assert.equal(form.barge_in_ms, "400");
  for (const value of [300, 400, 600]) {
    const next = { ...form, barge_in: true, barge_in_ms: String(value) };
    assert.equal(localSettingsError(next), "");
    assert.equal(localSettingsPayload(next).barge_in_ms, value);
    assert.equal(localSettingsPayload(next).barge_in, true);
  }
  for (const value of ["", "299", "601", "400.5", "NaN", "Infinity"]) {
    assert.match(localSettingsError({ ...form, barge_in_ms: value }), /插话时长/);
  }
});

test("Jev 语音筛选默认关闭，四个子开关独立往返且耗时阶段可导出", () => {
  const form = localSettingsForm();
  assert.equal(form.jev_filter_enabled, false);
  assert.equal(form.jev_unfinished_hold, true);
  assert.equal(form.jev_fast_route, true);
  assert.equal(form.jev_tool_prune, true);
  assert.equal(form.jev_memory_filter, true);
  const payload = localSettingsPayload({ ...form, jev_filter_enabled: true, jev_memory_filter: false });
  assert.equal(payload.jev_filter_enabled, true);
  assert.equal(payload.jev_tool_prune, true);
  assert.equal(payload.jev_memory_filter, false);
});

test("本地模型可选：分组、改选合成模型换默认音色、按模型检查音色范围", () => {
  const models = [
    { id: "silero_vad", kind: "vad" },
    { id: "ten_vad", kind: "vad" },
    { id: "sense_voice", kind: "asr" },
    { id: "x_asr_streaming", kind: "asr", streaming: true },
    { id: "kokoro", kind: "tts", voices: { default: 50, max: 53, hint: "45–48 中文女声" } },
    { id: "kokoro_v11", kind: "tts", voices: { default: 3, max: 102, hint: "3–57 中文女声" } },
    { id: "matcha_zh_en", kind: "tts" },
  ];
  const groups = localModelGroups(models);
  assert.deepEqual(groups.map((group) => [group.kind, group.models.map((model) => model.id)]), [
    ["vad", ["silero_vad", "ten_vad"]],
    ["asr", ["sense_voice", "x_asr_streaming"]],
    ["tts", ["kokoro", "kokoro_v11", "matcha_zh_en"]],
  ]);
  assert.deepEqual(localModelGroups(null).map((group) => group.models.length), [0, 0, 0]);

  const form = localSettingsForm({ tts: true, tts_speaker: 50 });
  // 识别、判停改选不动音色
  assert.equal(selectLocalModel(form, models, "asr", "x_asr_streaming").asr_model, "x_asr_streaming");
  assert.equal(selectLocalModel(form, models, "vad", "ten_vad").tts_speaker, "50");
  // 改选 Kokoro v1.1：音色换成它的默认值 3；再选回原模型不改
  const v11 = selectLocalModel(form, models, "tts", "kokoro_v11");
  assert.deepEqual([v11.tts_model, v11.tts_speaker], ["kokoro_v11", "3"]);
  assert.equal(selectLocalModel(v11, models, "tts", "kokoro_v11").tts_speaker, "3");
  // 单音色模型：保留编号（后端忽略），不检查范围
  const matcha = selectLocalModel(v11, models, "tts", "matcha_zh_en");
  assert.deepEqual([matcha.tts_model, matcha.tts_speaker], ["matcha_zh_en", "3"]);
  assert.equal(selectedVoices(matcha, models), null);
  assert.equal(localSettingsError({ ...matcha, tts_speaker: "500" }, models), "");
  // 多音色模型按各自上限检查
  assert.equal(localSettingsError({ ...v11, tts_speaker: "102" }, models), "");
  assert.match(localSettingsError({ ...v11, tts_speaker: "103" }, models), /0 ~ 102/);
  assert.match(localSettingsError({ ...form, tts_speaker: "54" }, models), /0 ~ 53/);
  assert.equal(selectedVoices(form, models).default, 50);
});

test("NV7: 豆包流式的表单、校验与指纹", () => {
  // 切到豆包：填入默认地址与资源 ID，保留密钥与音色；再切回清掉豆包地址
  const tts = { url: "https://h/v1", api_key: "k", model: "tts-1", voice: "v", speed: "1.2" };
  const doubao = switchKind("tts", tts, "doubao");
  assert.deepEqual(doubao, { ...tts, kind: "doubao", url: DOUBAO_DEFAULTS.tts.url, model: DOUBAO_DEFAULTS.tts.model });
  const back = switchKind("tts", doubao, "openai");
  assert.equal(back.kind, undefined);
  assert.equal(back.url, "");
  assert.equal(back.model, "");
  assert.equal(back.voice, "v");
  // 提交内容带 kind；OpenAI 兼容不带（外部服务端向导不受影响）
  assert.equal(sectionPayload("tts", doubao).kind, "doubao");
  assert.equal("kind" in sectionPayload("tts", tts), false);
  // 地址留空用默认地址；资源 ID 可留空
  assert.equal(finalUrl("asr", "", "doubao"), DOUBAO_DEFAULTS.asr.url);
  assert.equal(finalUrl("asr", "wss://x/y/", "doubao"), "wss://x/y");
  assert.equal(sectionInputError("asr", { kind: "doubao", url: "", model: "", api_key: "k" }), "");
  assert.equal(sectionInputError("asr", { kind: "doubao", url: "https://h", model: "" }), "豆包地址必须以 wss:// 开头");
  assert.equal(sectionInputError("tts", { ...doubao, voice: "" }), "请填写豆包音色");
  assert.equal(sectionInputError("tts", { ...doubao, speed: "3" }), "豆包语速须在 0.5 ~ 2.0 之间");
  assert.equal(sectionInputError("llm", { kind: "doubao", url: "", model: "" }), "大模型只支持 OpenAI 兼容接口");
  // 接口类型参与指纹：换类型后必须重新测试
  assert.notEqual(sectionFingerprint("tts", doubao), sectionFingerprint("tts", { ...doubao, kind: undefined }));
  // 已保存的豆包设置预填表单
  assert.equal(builtinModelForm("asr", { configured: true, kind: "doubao", url: DOUBAO_DEFAULTS.asr.url, model: "r" }).kind, "doubao");
  assert.equal(builtinModelForm("asr", { configured: true, kind: "openai", url: "https://h", model: "m" }).kind, undefined);
});

test("NV8: 接入点状态、身份、工具开关名单与新增校验", () => {
  assert.equal(endpointLabel("primary"), "主接入点");
  assert.equal(endpointLabel("extra-3"), "附加接入点 3");

  assert.deepEqual(endpointView({ active: true, connected: true, connections: 3, client: "xiaozhi v2" }), {
    tone: "ok",
    text: "已连接",
    detail: "对方：xiaozhi v2 · 已自动重连 2 次",
  });
  assert.equal(endpointView({ active: true, connected: true, connections: 1 }).detail, "");
  const failing = endpointView({ active: true, connected: false, connections: 1, last_error: "连接被拒绝" });
  assert.equal(failing.tone, "bad");
  assert.match(failing.detail, /连接被拒绝/);
  assert.equal(endpointView({ active: true, connected: false }).text, "连接中");
  const paused = endpointView({ active: false }, true);
  assert.equal(paused.text, "已暂停");
  assert.match(paused.detail, /自建模式/);

  const identity = identityView({ fields: { agentId: "agent-7" }, expires_at: 1000 }, 2000);
  assert.equal(identity.expired, true);
  assert.match(identity.text, /agentId agent-7/);
  assert.match(identity.text, /过期/);
  assert.match(identityView({ fields: {}, expires_at: 3000 }, 2000).text, /有效期至/);
  assert.deepEqual(identityView(null), { text: "", expired: false });

  const tools = [
    { name: "b_tool", enabled: true, can_disable: true },
    { name: "a_tool", enabled: false, can_disable: true },
    { name: "laser_safe_action_tool", enabled: true, can_disable: false },
  ];
  assert.deepEqual(toolSwitchPayload(tools, "b_tool", false), ["a_tool", "b_tool"]);
  assert.deepEqual(toolSwitchPayload(tools, "a_tool", true), []);
  assert.deepEqual(toolSwitchPayload(tools, "laser_safe_action_tool", false), ["a_tool"], "停止类工具不进入关闭名单");
  assert.deepEqual(toolSwitchPayload(tools, "unknown_tool", false), ["a_tool"]);

  assert.equal(toolStatText({ calls: 0 }), "本次运行未调用");
  assert.equal(toolStatText({ calls: 5, failures: 1, denied: 2 }), "调用 5 次 · 失败 1 · 拒绝 2");

  assert.equal(extraEndpointError(""), "请填写接入点地址");
  assert.match(extraEndpointError("http://x/"), /ws:\/\//);
  assert.match(extraEndpointError("wss://x/?token=<your-token>"), /占位符/);
  const full = [1, 2, 3, 4].map((n) => ({ id: `extra-${n}` })).concat([{ id: "primary" }]);
  assert.match(extraEndpointError("wss://x/mcp/?token=t", full, 4), /最多 4 个/);
  assert.equal(extraEndpointError("wss://x/mcp/?token=t", [{ id: "primary" }], 4), "");
});

test("UI-3 设置分组：只提交本组字段，带令牌的字段留空不提交，端口转数字", () => {
  const form = {
    server_dir: " D:/xz ", python: "py", args: "app.py", server_port: "8000",
    builtin_ws_port: "8100", builtin_http_port: "8103",
    self_hosted_mcp_endpoint: "", endpoint_health_url: "  ", official_mcp_endpoint: " wss://example.test/mcp/?token=t ",
  };
  assert.deepEqual(settingsGroupPayload("official", form), { official_mcp_endpoint: "wss://example.test/mcp/?token=t" });
  assert.deepEqual(settingsGroupPayload("selfHostedEndpoint", form), {});
  assert.deepEqual(settingsGroupPayload("builtinPorts", form), { builtin_ws_port: 8100, builtin_http_port: 8103 });
  // n71 常连模式：开关按布尔值提交，端口转数字
  assert.deepEqual(
    settingsGroupPayload("builtinMqtt", { builtin_mqtt_enabled: "true", builtin_mqtt_port: "1884", builtin_udp_port: " 8890 " }),
    { builtin_mqtt_enabled: true, builtin_mqtt_port: 1884, builtin_udp_port: 8890 },
  );
  assert.equal(settingsGroupPayload("builtinMqtt", { builtin_mqtt_enabled: "false", builtin_mqtt_port: "1883", builtin_udp_port: "8888" }).builtin_mqtt_enabled, false);
  assert.ok(settingsGroupDirty("builtinMqtt", { builtin_mqtt_enabled: "true", builtin_mqtt_port: "1883", builtin_udp_port: "8888" }, { builtin_mqtt_enabled: "false", builtin_mqtt_port: "1883", builtin_udp_port: "8888" }));
  assert.match(settingsSaveEffect("builtinMqtt", "builtin", false), /小智要再重启一次/);
  assert.deepEqual(settingsGroupPayload("selfHostedServer", form), { server_dir: "D:/xz", python: "py", args: "app.py", server_port: 8000 });
  // 各组字段互不重叠：保存一组不会顺带提交另一组
  const keys = Object.values(SETTINGS_GROUPS).flatMap((group) => group.keys);
  assert.equal(new Set(keys).size, keys.length);
});

test("UI-3 设置分组改动判断：普通字段与已保存值比较，令牌字段填了才算改动", () => {
  const saved = { server_dir: "D:/xz", python: "", args: "app.py", server_port: "8000", builtin_ws_port: "8000", builtin_http_port: "8003", self_hosted_mcp_endpoint: "", endpoint_health_url: "", official_mcp_endpoint: "" };
  assert.equal(settingsGroupDirty("selfHostedServer", { ...saved, server_dir: " D:/xz " }, saved), false);
  assert.equal(settingsGroupDirty("selfHostedServer", { ...saved, server_port: "8001" }, saved), true);
  assert.equal(settingsGroupDirty("builtinPorts", { ...saved, server_port: "8001" }, saved), false);
  assert.equal(settingsGroupDirty("official", { ...saved, official_mcp_endpoint: "wss://x" }, saved), true);
  assert.equal(settingsGroupDirty("official", saved, saved), false);
});

test("UI-3 本地模型与语音体验分开保存：合并时只取本组草稿", () => {
  assert.equal(LOCAL_MODEL_KEYS.filter((key) => EXPERIENCE_KEYS.includes(key)).length, 0);
  const saved = localSettingsForm({});
  const draft = { ...saved, tts_speed: "1.5", barge_in: true };
  const local = mergeGroup(saved, draft, LOCAL_MODEL_KEYS);
  assert.equal(local.tts_speed, "1.5");
  assert.equal(local.barge_in, false, "保存本地模型时不带上未保存的体验改动");
  const experience = mergeGroup(saved, draft, EXPERIENCE_KEYS);
  assert.equal(experience.barge_in, true);
  assert.equal(experience.tts_speed, saved.tts_speed);
  assert.equal(formDirty(saved, draft, LOCAL_MODEL_KEYS), true);
  assert.equal(formDirty(saved, { ...saved }, EXPERIENCE_KEYS), false);
  assert.equal(formDirty(null, draft), false);
  // 只出现在表单里的字段（如切到豆包时加上的 kind）也算改动
  assert.equal(formDirty({ url: "u", model: "m" }, { url: "u", model: "m", kind: "doubao" }), true);
  // n69：画风只在“设备 › 副屏预览”、Jev 开关只在顶部混搭栏编辑，对话体验组的草稿和保存载荷都不含这两个键
  for (const key of ["screen_card_style", "jev_filter_enabled"]) {
    assert.equal(EXPERIENCE_KEYS.includes(key), false, `${key} 不属于对话体验组`);
    assert.equal(LOCAL_MODEL_KEYS.includes(key), false, `${key} 不属于本地模型组`);
  }
  const otherEdits = { ...saved, screen_card_style: "goggle_bot", jev_filter_enabled: true };
  assert.equal(formDirty(saved, otherEdits, EXPERIENCE_KEYS), false, "外部改了画风或 Jev 开关不会让对话体验组显示未保存");
  assert.deepEqual(localGroupPatch(saved, { ...otherEdits, barge_in: true }, EXPERIENCE_KEYS), { barge_in: true }, "对话体验保存不回写画风与 Jev 开关");
  assert.equal(formDirty({ url: "u", kind: "doubao" }, { url: "u" }), true);
});

test("UI-3 两组本地设置各自只检查本组字段", () => {
  const saved = localSettingsForm({});
  const badHotword = { ...saved, custom_hotwords: "木".repeat(33) };
  assert.match(experienceSettingsError(badHotword), /32/);
  assert.equal(localModelSettingsError(badHotword), "", "体验组的错误不挡本地模型组保存");
  const badSpeed = { ...saved, tts_speed: "3" };
  assert.match(localModelSettingsError(badSpeed), /0.5/);
  assert.equal(experienceSettingsError(badSpeed), "", "本地模型组的错误不挡体验组保存");
});

test("UI-3 连接设置保存后，只有后端真的重新发起连接才说正在重新连接", () => {
  assert.equal(settingsSaveEffect("selfHostedEndpoint", "self_hosted", true), "正在重新连接自建接入点");
  assert.equal(settingsSaveEffect("selfHostedEndpoint", "self_hosted", false), "接入点未变，没有重新连接");
  assert.equal(settingsSaveEffect("selfHostedEndpoint", "official", false), "切到自建链路时使用");
  assert.equal(settingsSaveEffect("official", "official", true), "正在重新连接小智");
  assert.equal(settingsSaveEffect("official", "builtin", false), "接入点未变，没有重新连接");
  assert.equal(settingsSaveEffect("official", "self_hosted", false), "切回官方链路时使用");
  assert.equal(settingsSaveEffect("builtinPorts", "builtin", false), "重启内置服务后生效");
  assert.equal(settingsSaveEffect("selfHostedServer", "self_hosted", false), "下次启动服务端时生效");
});

test("UI-3 保存结果、未保存标签与分组摘要", () => {
  const at = new Date(2026, 9, 14, 9, 5, 7);
  assert.equal(savedNoteText("大模型设置", "下次启动内置服务时生效", at), "已保存大模型设置 · 09:05:07 · 下次启动内置服务时生效");
  assert.equal(savedNoteText("路径", "", at), "已保存路径 · 09:05:07");
  assert.deepEqual([...dirtyTabs({ a: "models", b: "", c: "models", d: "connect" })].sort(), ["connect", "models"]);
  assert.equal(dirtyTabs(null).size, 0);
  assert.equal(builtinModelSummary("llm", null), "未配置");
  assert.equal(builtinModelSummary("llm", { configured: true, model: "m1" }), "已配置 · m1");
  assert.equal(builtinModelSummary("tts", { configured: true, kind: "doubao", model: "r" }), "已配置 · 豆包流式 · r");
  assert.equal(builtinModelSummary("asr", null, true), "使用本地模型");
  assert.equal(builtinModelSummary("llm", null, true), "未配置", "大模型没有本地选项");
  assert.equal(externalModelSummary(null), "未测试");
  assert.equal(externalModelSummary({ ok: false }), "上次测试未通过");
});

test("声纹简易标定：至少两人且每人三段测试音才能计算；名单变化提示重新标定", async () => {
  const { speakerCalibrationView } = await import("../src/voiceState.js");
  const speakers = [{ id: "a" }, { id: "b" }];
  const none = speakerCalibrationView({ speakers: [{ id: "a" }], test_clips: { a: 3 }, calibration: { state: "none" } });
  assert.equal(none.ready, false);
  assert.equal(none.blocked, "至少需要两个已登记的人");
  assert.match(none.status, /未标定/);
  const partial = speakerCalibrationView({ speakers, test_clips: { a: 3, b: 2 } });
  assert.equal(partial.ready, false);
  assert.deepEqual(partial.counts, { a: 3, b: 2 });
  const ready = speakerCalibrationView({ speakers, test_clips: { a: 3, b: 3 } });
  assert.equal(ready.ready, true);
  const report = { threshold: 0.6123, people: 2, genuine_accepted: 6, genuine_trials: 6, false_accepts: 0, impostor_trials: 6 };
  const valid = speakerCalibrationView({ speakers, calibration: { state: "valid", report } });
  assert.match(valid.status, /阈值 0\.61 · 本人认对 6\/6 · 认成别人 0\/6/);
  assert.match(speakerCalibrationView({ speakers, calibration: { state: "stale", report } }).status, /需要重新标定/);
});

test("固件推送记录：只有设备上报新版本才算升级；到期或取消后不能再取消", async () => {
  const { firmwareDeliveryView } = await import("../src/voiceState.js");
  const base = { device_report_status: "not_reported", download_status: "pending", expires_at: 200 };
  assert.deepEqual(firmwareDeliveryView(base, 100), { text: "等待设备检查更新（重启设备即可）", cancellable: true });
  assert.match(firmwareDeliveryView({ ...base, download_status: "downloading" }, 100).text, /已开始下载/);
  assert.equal(firmwareDeliveryView({ ...base, download_status: "downloading" }, 100).cancellable, true);
  assert.match(firmwareDeliveryView({ ...base, download_status: "unknown" }, 100).text, /结果未知/);
  assert.match(firmwareDeliveryView(base, 300).text, /已失效/);
  assert.equal(firmwareDeliveryView(base, 300).cancellable, false);
  assert.match(firmwareDeliveryView({ ...base, download_status: "downloading" }, 300).text, /核对设备当前版本/);
  assert.match(firmwareDeliveryView({ ...base, device_report_status: "reported", download_status: "downloaded" }, 300).text, /已升级/);
  assert.match(firmwareDeliveryView({ device_report_status: "not_reported", download_status: "pending", expires_at: 0 }, 1).text, /已失效/, "旧记录没有到期时间");
});

test("Agent 卡：严格模式未开时提示；CLI 不可用时给原因；运行记录可读", async () => {
  const { agentView } = await import("../src/voiceState.js");
  assert.equal(agentView({ configured: false }).summary, "未开启");
  const noStrict = agentView({ configured: true, strict_mode: false, providers: [{ provider: "codex", ready: false, error_code: "agent_strict_mode_required" }] });
  assert.equal(noStrict.summary, "已开启 · 需要严格模式");
  assert.equal(noStrict.strictMissing, true);
  assert.equal(noStrict.providers[0].text, "需要先开启严格模式");
  const ready = agentView({ configured: true, strict_mode: true, providers: [
    { provider: "claude_code", ready: true, version: "2.1.283 (Claude Code)" },
    { provider: "codex", ready: true, version: "codex-cli 0.157.1", source: "auto" },
  ],
    runs: [{ id: "r", status: "cancelled", error_code: "agent_stopped", output: "" }, { id: "s", status: "running", stoppable: true }] });
  assert.equal(ready.summary, "可用");
  assert.equal(ready.providers[0].text, "可用（2.1.283 (Claude Code)）");
  assert.equal(ready.providers[1].text, "可用（codex-cli 0.157.1） · 自动读取");
  const fallback = agentView({ configured: true, strict_mode: true, providers: [{ provider: "codex", ready: true, version: "codex-cli 0.157.1", source: "auto_fallback" }] });
  assert.equal(fallback.providers[0].text, "可用（codex-cli 0.157.1） · 已自动适配当前设备");
  assert.equal(ready.runs[0].statusText, "已停止");
  assert.equal(ready.runs[0].detail, "已手动停止");
  assert.equal(ready.runs[1].stoppable, true);
});

test("内置云端模型：全量模型列表地址推导、自动拉取条件与下拉多词查询过滤", async () => {
  const {
    DOUBAO_MODEL_OPTIONS,
    canAutoFetchModels,
    filterModelOptions,
    modelsListError,
    modelsListKey,
    modelsListUrl,
  } = await import("../src/voiceState.js");
  assert.equal(modelsListUrl(" https://api.example.test/v1/ "), "https://api.example.test/v1/models");
  assert.equal(modelsListUrl("https://api.example.test/v1/chat/completions"), "https://api.example.test/v1/models");
  assert.equal(modelsListUrl("https://api.example.test/v1/audio/transcriptions/"), "https://api.example.test/v1/models");
  assert.equal(modelsListUrl("https://api.example.test/v1/audio/speech"), "https://api.example.test/v1/models");
  assert.equal(modelsListUrl("https://api.example.test/v1/models"), "https://api.example.test/v1/models");

  assert.equal(modelsListError({ url: "" }), "请先填写地址");
  assert.equal(modelsListError({ url: "wss://x" }), "地址必须以 http:// 或 https:// 开头");
  assert.equal(modelsListError({ kind: "doubao", url: "" }), "");

  assert.equal(canAutoFetchModels({ url: "https://api.example.test/v1", api_key: "" }, { key_set: false }), false, "公有云未填密钥时不自动盲发");
  assert.equal(canAutoFetchModels({ url: "https://api.example.test/v1", api_key: "sk-x" }, { key_set: false }), true);
  assert.equal(canAutoFetchModels({ url: "https://api.example.test/v1", api_key: "" }, { key_set: true }), true, "本机已存密钥可直接自动拉取");
  assert.equal(canAutoFetchModels({ url: "http://127.0.0.1:11434/v1", api_key: "" }, { key_set: false }), true, "本机回环服务免密钥可自动拉取");
  assert.equal(canAutoFetchModels({ kind: "doubao", url: "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async", api_key: "k" }, {}), false);
  assert.notEqual(
    modelsListKey({ url: "https://a.example.test/v1", api_key: "k1" }, {}),
    modelsListKey({ url: "https://a.example.test/v1", api_key: "k2" }, {}),
  );

  const remote = [
    "cline-pass/deepseek-v4.1-flash",
    "Qwen/Qwen2.5-7B-Instruct",
    "FunAudioLLM/SenseVoiceSmall",
    "MOSS/CosyVoice2-0.5B",
    "Qwen/Qwen2.5-Omni-7B",
    "Qwen/Qwen2-Audio-7B-Instruct",
    "BAAI/bge-m3",
  ];
  const all = filterModelOptions(remote, "Qwen/Qwen2.5-7B-Instruct", false);
  assert.equal(all.length, 7, "未开启分区过滤时返回全部模型");
  assert.equal(all[0].hint, "ClinePass 订阅");
  assert.equal(all[0].displayHint, "ClinePass 订阅 · 大模型");
  assert.equal(all[2].displayHint, "语音识别");
  assert.equal(all[3].displayHint, "语音合成");
  assert.equal(all[4].displayHint, "大模型 · 语音识别 · 语音合成", "Omni 全能模型标注三项用途");
  assert.equal(all[5].displayHint, "大模型 · 语音识别", "Audio-Instruct 标注两项用途");

  // 按分区自动筛选：多能力模型同时出现在它支持的多个分区里，向量模型不污染任何分区
  assert.deepEqual(
    filterModelOptions(remote, "", false, { section: "llm", onlyMatched: true }).map((item) => item.id),
    ["cline-pass/deepseek-v4.1-flash", "Qwen/Qwen2.5-7B-Instruct", "Qwen/Qwen2.5-Omni-7B", "Qwen/Qwen2-Audio-7B-Instruct"],
  );
  assert.deepEqual(
    filterModelOptions(remote, "", false, { section: "asr", onlyMatched: true }).map((item) => item.id),
    ["FunAudioLLM/SenseVoiceSmall", "Qwen/Qwen2.5-Omni-7B", "Qwen/Qwen2-Audio-7B-Instruct"],
  );
  assert.deepEqual(
    filterModelOptions(remote, "", false, { section: "tts", onlyMatched: true }).map((item) => item.id),
    ["MOSS/CosyVoice2-0.5B", "Qwen/Qwen2.5-Omni-7B"],
  );
  assert.deepEqual(
    filterModelOptions(remote, "订阅 flash", true).map((item) => item.id),
    ["cline-pass/deepseek-v4.1-flash"],
  );
  assert.deepEqual(
    filterModelOptions(remote, "sense small", true).map((item) => item.id),
    ["FunAudioLLM/SenseVoiceSmall"],
  );
  assert.ok(DOUBAO_MODEL_OPTIONS.asr.length >= 2 && DOUBAO_MODEL_OPTIONS.tts.length >= 2);
});

test("内置模型自由混搭视图与一键切换云端/本地载荷（云端配置保留备用）", async () => {
  const { builtinSectionSourceView, builtinSourceSwitchPayload } = await import("../src/voiceState.js");
  const status = {
    models: {
      llm: { configured: true, url: "https://llm.test/v1", model: "qwen-max", key_set: true },
      asr: { configured: true, url: "https://asr.test/v1/audio/transcriptions", model: "FunAudioLLM/SenseVoiceSmall", key_set: true },
      tts: { configured: true, url: "https://tts.test/v1/audio/speech", model: "MOSS/TTS-1", voice: "alloy", speed: 1, key_set: true },
    },
    local: {
      settings: { asr: false, tts: false, vad: false, asr_model: "sense_voice", tts_model: "kokoro", tts_speaker: 50, tts_speed: 1 },
      models: [
        { id: "silero_vad", kind: "vad", label: "Silero VAD", installed: true },
        { id: "ten_vad", kind: "vad", label: "TEN-VAD", installed: false },
        { id: "sense_voice", kind: "asr", label: "SenseVoice 2024", installed: false },
        { id: "kokoro", kind: "tts", label: "Kokoro v1.0", installed: false, voices: { default: 50, max: 53 } },
        { id: "kokoro_v11", kind: "tts", label: "Kokoro v1.1", installed: true, voices: { default: 3, max: 102 } },
      ],
    },
  };
  // 默认三项均走云端、判停走内置能量判停；tts 虽然当前选中的 kokoro 未安装，但同类别 kokoro_v11 已安装
  assert.equal(builtinSectionSourceView("llm", status).activeText, "云端（qwen-max）");
  assert.equal(builtinSectionSourceView("asr", status).activeText, "云端（FunAudioLLM/SenseVoiceSmall）");
  const vadEnergyView = builtinSectionSourceView("vad", status);
  assert.equal(vadEnergyView.source, "energy");
  assert.equal(vadEnergyView.selectValue, "energy");
  assert.equal(vadEnergyView.activeText, "内置能量判停（免下载）");
  assert.deepEqual(
    vadEnergyView.options.map((opt) => [opt.value, opt.label, opt.installed]),
    [
      ["energy", "内置能量判停（免下载）", true],
      ["local:silero_vad", "本地 · Silero VAD", true],
      ["local:ten_vad", "本地 · TEN-VAD（未下载）", false],
    ],
  );
  const toLocalVad = builtinSourceSwitchPayload(status, "vad", "local:silero_vad");
  assert.equal(toLocalVad.vad, true);
  assert.equal(toLocalVad.vad_model, "silero_vad");
  const backToEnergyVad = builtinSourceSwitchPayload({ ...status, local: { ...status.local, settings: toLocalVad } }, "vad", "energy");
  assert.equal(backToEnergyVad.vad, false);
  assert.equal(builtinSectionSourceView("llm", status).activeText, "云端（qwen-max）");
  assert.equal(builtinSectionSourceView("asr", status).activeText, "云端（FunAudioLLM/SenseVoiceSmall）");
  const ttsCloudView = builtinSectionSourceView("tts", status);
  assert.equal(ttsCloudView.source, "cloud");
  assert.equal(ttsCloudView.selectValue, "cloud");
  assert.equal(ttsCloudView.installedLocalModel?.id, "kokoro_v11");
  assert.deepEqual(
    ttsCloudView.options.map((opt) => [opt.value, opt.label, opt.installed]),
    [
      ["cloud", "云端（MOSS/TTS-1）", true],
      ["local:kokoro", "本地 · Kokoro v1.0（未下载）", false],
      ["local:kokoro_v11", "本地 · Kokoro v1.1", true],
    ],
  );

  // 从顶部下拉框直接选择 local:kokoro_v11：自动改选 kokoro_v11 与默认音色 3，且不动识别开关
  const toLocalTts = builtinSourceSwitchPayload(status, "tts", "local:kokoro_v11");
  assert.equal(toLocalTts.tts, true);
  assert.equal(toLocalTts.asr, false);
  assert.equal(toLocalTts.tts_model, "kokoro_v11");
  assert.equal(toLocalTts.tts_speaker, 3);

  // 切换后：语音识别走云端、语音合成走本地、大模型走云端，且云端 TTS 配置仍保留在 status.models.tts
  const mixedStatus = {
    ...status,
    local: { ...status.local, settings: toLocalTts },
  };
  const ttsLocalView = builtinSectionSourceView("tts", mixedStatus);
  assert.equal(ttsLocalView.source, "local");
  assert.equal(ttsLocalView.selectValue, "local:kokoro_v11");
  assert.equal(ttsLocalView.localReady, true);
  assert.equal(ttsLocalView.cloudConfigured, true);
  assert.equal(ttsLocalView.activeText, "本地（Kokoro v1.1）");

  // 从顶部下拉框选回 cloud
  const backToCloud = builtinSourceSwitchPayload(mixedStatus, "tts", "cloud");
  assert.equal(backToCloud.tts, false);
  assert.equal(backToCloud.tts_model, "kokoro_v11");

  // 按真实语音执行链路排序：拾音判停(vad) -> 语音识别(asr) -> Jev 前置粗筛(jev) -> 大模型(llm) -> 语音合成(tts)
  const { MIX_SECTIONS, builtinJevSummary, jevModelError } = await import("../src/voiceState.js");
  assert.deepEqual(MIX_SECTIONS.map((s) => s.key), ["vad", "asr", "jev", "llm", "tts"]);
  const jevUnconfigured = builtinSectionSourceView("jev", status);
  assert.equal(jevUnconfigured.selectValue, "off");
  assert.equal(jevUnconfigured.options[1].installed, false);
  assert.match(jevUnconfigured.options[1].label, /需配密钥/);
  const readyJevStatus = {
    ...status,
    jev: { key_set: true, model: "typesafe/jev-1.14", voice_enabled: false, feedback_enabled: true },
    local: { ...status.local, jev_key_configured: true },
  };
  assert.equal(builtinJevSummary(readyJevStatus), "加工反馈开（固定 1.13） · 语音待用 typesafe/jev-1.14");
  const switchTo113 = builtinSourceSwitchPayload(readyJevStatus, "jev", "jev:typesafe/jev-1.13");
  assert.equal(switchTo113.jev_filter_enabled, true);
  assert.equal(switchTo113.jev_model, "typesafe/jev-1.13");
  const switchOff = builtinSourceSwitchPayload({ ...readyJevStatus, local: { ...readyJevStatus.local, settings: switchTo113 } }, "jev", "off");
  assert.equal(switchOff.jev_filter_enabled, false);
  assert.equal(switchOff.jev_model, "typesafe/jev-1.13", "关闭 Jev 筛选时保留已选模型版本作备用");
  assert.equal(jevModelError("typesafe/jev-1.14"), "");
  assert.match(jevModelError("laya_multilingual"), /typesafe\/jev-/);
  assert.match(jevModelError("gpt-4o"), /typesafe\/jev-/);
  const switchToLocalLaya = builtinSourceSwitchPayload(readyJevStatus, "jev", "local:laya_multilingual");
  assert.equal(switchToLocalLaya.jev_filter_enabled, true);
  assert.equal(switchToLocalLaya.jev_model, "laya_multilingual");
  const localLayaReadyStatus = {
    ...status,
    jev: { key_set: false, local_installed: true, model: "laya_multilingual", voice_enabled: true, feedback_enabled: false },
    local: {
      ...status.local,
      jev_key_configured: true,
      settings: { ...status.local.settings, jev_filter_enabled: true, jev_model: "laya_multilingual" },
    },
  };
  assert.equal(builtinJevSummary(localLayaReadyStatus), "使用本地模型");
  assert.equal(builtinSectionSourceView("jev", localLayaReadyStatus).selectValue, "local:laya_multilingual");
});
test("同源服务商地址/密钥复用、服务端 model_sections 融合与 TTS 常用音色预设", async () => {
  const {
    applyDefaultTtsVoice,
    canAutoFetchModels,
    chatOnlyGatewayName,
    filterModelOptions,
    finalUrl,
    findPeerCloudSource,
    sectionInputError,
    ttsVoiceOptionsFor,
  } = await import("../src/voiceState.js");
  const status = {
    models: {
      llm: { configured: true, url: "https://api.siliconflow.cn/v1", model: "Qwen/Qwen2.5-7B-Instruct", key_set: true },
    },
  };
  // 未填地址时可一键复用大模型地址与密钥；填入同一基础地址后识别为同源并可免重填密钥自动拉取
  const emptyPeer = findPeerCloudSource("asr", status, "");
  assert.equal(emptyPeer.reusable?.baseUrl, "https://api.siliconflow.cn/v1");
  assert.equal(emptyPeer.reusable?.keySet, true);
  assert.equal(emptyPeer.sameOrigin, null);

  const samePeer = findPeerCloudSource("asr", status, "https://api.siliconflow.cn/v1/audio/transcriptions");
  assert.equal(samePeer.sameOrigin?.section, "llm");
  assert.equal(
    canAutoFetchModels({ url: "https://api.siliconflow.cn/v1", api_key: "" }, { key_set: false }, status, "asr"),
    true,
    "同源地址直接复用已配置分区的密钥自动拉取模型列表",
  );
  // Cline / OpenRouter / DeepSeek 等仅对话网关：不对语音识别/合成暴露“一键复用”，手动填入时报错且不拼凑 /audio/transcriptions
  const clineStatus = {
    models: {
      llm: { configured: true, url: "https://api.cline.bot/api/v1", model: "cline-pass/deepseek-v4.1-flash", key_set: true },
    },
  };
  assert.equal(chatOnlyGatewayName("https://api.cline.bot/api/v1"), "Cline (api.cline.bot)");
  assert.equal(findPeerCloudSource("asr", clineStatus, "").reusable, null, "Cline 仅支持大模型，不在 ASR 提供复用按钮");
  assert.equal(findPeerCloudSource("tts", clineStatus, "").reusable, null, "Cline 仅支持大模型，不在 TTS 提供复用按钮");
  assert.equal(finalUrl("asr", "https://api.cline.bot/api/v1", "openai"), "", "仅对话网关在 ASR 不拼凑虚假 /audio/transcriptions");
  assert.match(
    sectionInputError("asr", { url: "https://api.cline.bot/api/v1", api_key: "", model: "whisper-1" }),
    /仅提供大模型对话接口/,
  );
  // 当服务商返回的全部是纯文本大模型时，ASR/TTS 的 onlyMatched 筛选返回空数组，绝不把纯文本大模型静默冒充语音模型列出
  assert.deepEqual(
    filterModelOptions(["cline-pass/deepseek-v4.1-flash", "Qwen/Qwen2.5-7B-Instruct"], "", false, {
      section: "asr",
      onlyMatched: true,
    }),
    [],
  );
  // 服务端返回原生 model_sections 时，即使模型名字不带 tts/asr 也能零误差筛出
  const customModels = ["vendor/custom-voice-v1", "vendor/custom-embed-v1"];
  const modelSections = {
    "vendor/custom-voice-v1": ["tts"],
    "vendor/custom-embed-v1": [],
  };
  assert.deepEqual(
    filterModelOptions(customModels, "", false, { section: "tts", onlyMatched: true, modelSections }).map((m) => m.id),
    ["vendor/custom-voice-v1"],
  );

  // TTS 常用音色预设与选模型自动填默认音色（CosyVoice 动态带模型名前缀）
  const cosyForm = applyDefaultTtsVoice({ url: "https://api.siliconflow.cn/v1", model: "", voice: "", speed: "1" }, "FunAudioLLM/CosyVoice2-0.5B");
  assert.equal(cosyForm.voice, "FunAudioLLM/CosyVoice2-0.5B:anna");
  assert.ok(ttsVoiceOptionsFor(cosyForm).some((opt) => opt.id === "FunAudioLLM/CosyVoice2-0.5B:alex"));
  const switchedCosy = applyDefaultTtsVoice(cosyForm, "FunAudioLLM/CosyVoice-300M");
  assert.equal(switchedCosy.voice, "FunAudioLLM/CosyVoice-300M:anna", "切换 CosyVoice 模型时同步更新音色前缀");
  const openaiForm = applyDefaultTtsVoice(switchedCosy, "tts-1");
  assert.equal(openaiForm.voice, "alloy", "切回 OpenAI 合成模型时换成 alloy 默认音色");
});

test("内置工具增删提示词：给出真实改代码路径、Jev 粗筛归组并与第三方 MCP 区分（2026-11-02）", () => {
  const add = toolEditPrompt("add");
  assert.match(add, /crates\/laser-tools\/src\/lib\.rs/, "新增模板指向工具登记处");
  assert.match(add, /tests\/golden\/mcp_tools\.json/, "新增模板指向编译期嵌入的黄金工具清单");
  assert.match(add, /tools_list_with/);
  assert.match(add, /dispatch_tool_call_with/);
  assert.match(add, /tool_risk/, "新增模板要求登记风险分级");
  assert.match(add, /groups_of_builtin_tool/, "新增模板要求归入既有 6 个 Jev 粗筛组而非逐工具平铺 noul");
  assert.match(add, /审批与严格模式门禁/, "新增工具不得放宽门禁");
  const remove = toolEditPrompt("remove");
  assert.match(remove, /取消勾选/, "先说明只关开关就能让模型看不到");
  assert.match(remove, /groups_of_builtin_tool/, "删除模板要求清理 Jev 静态归组残留");
  assert.match(remove, /laser_safe_action_tool/, "急停工具不可删除");
  assert.match(remove, /validate_disabled_tools/);
  assert.equal(toolEditPrompt("unknown"), "", "未知类型返回空字符串");
  assert.deepEqual(Object.keys(TOOL_EDIT_PROMPTS).sort(), ["add", "remove"]);
  for (const value of Object.values(TOOL_EDIT_PROMPTS)) {
    assert.ok(value.title && value.text.length > 200, `${value.title} 有标题与可复制的正文`);
  }
});

test("schema 14 设备信息快照：缺字段显示破折号，AEC 只认设备上报", async () => {
  const { builtinDeviceInfoView } = await import("../src/voiceState.js");
  const value = (view, label) => view.rows.find(([name]) => name === label)[1];

  const empty = builtinDeviceInfoView({});
  assert.equal(empty.hasInfo, false);
  assert.equal(value(empty, "固件版本"), "—");
  assert.equal(value(empty, "服务端 AEC 声明"), "未上报", "没有 hello 就不猜");

  const device = {
    firmware_version: "2.4.0",
    info_json: JSON.stringify({
      language: "zh-CN",
      flash_size: 16 * 1024 * 1024,
      psram_size: 8 * 1024 * 1024,
      chip_model_name: "esp32s3",
      mac_address: "aa:bb:cc:dd:ee:ff",
      uuid: "uuid-1",
      application: { name: "xiaozhi", version: "2.5.0", idf_version: "v6.1.0", compile_time: "Nov  1 2026" },
    }),
    hello_json: JSON.stringify({
      protocol_version: 1,
      transport: "websocket",
      aec: true,
      mcp: true,
      audio: { format: "opus", sample_rate: 24000, channels: 1, frame_duration: 60 },
    }),
    info_at: 12,
  };
  const view = builtinDeviceInfoView(device);
  assert.equal(value(view, "固件版本"), "2.5.0", "以上报的 application.version 为准");
  assert.equal(value(view, "服务端 AEC 声明"), "已声明");
  assert.equal(value(view, "Flash / PSRAM"), "16 MB / 8 MB");
  assert.equal(value(view, "上行音频"), "24000 Hz · opus · 60 ms");
  assert.equal(value(view, "MAC"), "aa:bb:cc:dd:ee:ff");
  assert.equal(view.updatedAt, 12);

  const off = builtinDeviceInfoView({ hello_json: JSON.stringify({ aec: false }) });
  assert.equal(value(off, "服务端 AEC 声明"), "未声明", "只有设备明确上报 false 才显示未声明");
  assert.equal(value(off, "固件版本"), "—", "没有版本就不猜");
  // 真实固件是设备端 AEC（CONFIG_USE_DEVICE_AEC），hello 里根本没有 aec 字段
  const none = builtinDeviceInfoView({ hello_json: JSON.stringify({ mcp: true }) });
  assert.equal(
    value(none, "服务端 AEC 声明"),
    "未上报",
    "设备没上报就不能显示成未声明，更不能当成设备端 AEC 已开启",
  );
  assert.equal(value(none, "设备端回声消除").includes("不推断"), true);
  // 实际拾音模式：判断设备端 AEC 的行为证据，只显示设备真的用过的模式
  const mode = (json) => value(builtinDeviceInfoView({ listen_json: JSON.stringify(json) }), "实际拾音模式");
  assert.equal(builtinDeviceInfoView({}).rows.find(([l]) => l === "实际拾音模式")[1], "未上报");
  assert.match(mode({ mode: "realtime" }), /设备端 AEC 开启/);
  assert.match(mode({ mode: "auto" }), /关闭或未启用/);
  assert.match(mode({ mode: "manual" }), /无法判断/);
  assert.equal(mode({ mode: "unknown-mode" }), "未上报", "不认识的模式不猜");
  assert.equal(builtinDeviceInfoView({ listen_json: "{oops" }).rows.length > 0, true);
  assert.equal(value(off, "固件版本"), "—", "没有版本就不猜");
  assert.equal(builtinDeviceInfoView({ info_json: "{oops" }).hasInfo, false, "坏 JSON 不抛异常");
});

test("顶部切换 Jev 开关后编辑并保存语音体验，不会把未手改的 Jev 开关覆盖回旧值", async () => {
  const {
    EXPERIENCE_KEYS,
    builtinJevForm,
    builtinSourceSwitchPatch,
    localGroupPatch,
    localSettingsForm,
    syncJevDraft,
    syncLocalDraft,
  } = await import("../src/voiceState.js");
  const initialBaseline = localSettingsForm({ jev_filter_enabled: false, barge_in_ms: 400 });
  // 顶部切换仅生成最小增量补丁，不携带无关字段
  assert.deepEqual(
    builtinSourceSwitchPatch({ local: { settings: initialBaseline } }, "jev", "jev:typesafe/jev-1.14"),
    { jev_filter_enabled: true, jev_model: "typesafe/jev-1.14" },
  );
  assert.deepEqual(
    builtinSourceSwitchPatch({ local: { settings: initialBaseline } }, "jev", "off"),
    { jev_filter_enabled: false },
  );

  // 用户尚未编辑体验卡片，通过顶部第 5 栏下拉框将 jev_filter_enabled 开启为 true
  const nextBaseline = localSettingsForm({ jev_filter_enabled: true, barge_in_ms: 400 });
  const syncedDraft = syncLocalDraft(initialBaseline, nextBaseline, initialBaseline, EXPERIENCE_KEYS);
  assert.equal(syncedDraft.jev_filter_enabled, true, "未手动编辑过的字段应跟随新 baseline 更新");

  // 随后用户仅修改插话时长并保存“语音体验”
  const editedDraft = { ...syncedDraft, barge_in_ms: "500" };
  const patch = localGroupPatch(nextBaseline, editedDraft, EXPERIENCE_KEYS);
  assert.deepEqual(patch, { barge_in_ms: 500 }, "局部保存只提交实际改动字段，绝不回写旧 jev_filter_enabled=false");

  // 反向场景：即使草稿里原本已手改了插话时长，外部关闭 Jev 后保存体验仍保持 Jev 关闭
  const preEdited = { ...nextBaseline, barge_in_ms: "520" };
  const closedBaseline = localSettingsForm({ jev_filter_enabled: false, barge_in_ms: 400 });
  const syncedAfterClose = syncLocalDraft(nextBaseline, closedBaseline, preEdited, EXPERIENCE_KEYS);
  assert.equal(syncedAfterClose.jev_filter_enabled, false, "外部关闭 Jev 必须同步到草稿，防止重新打开外传");
  assert.equal(syncedAfterClose.barge_in_ms, "520", "用户已手改的插话时长继续保留");
  assert.deepEqual(localGroupPatch(closedBaseline, syncedAfterClose, EXPERIENCE_KEYS), { barge_in_ms: 520 });

  // Jev 卡片在用户手填备用密钥或勾选反馈开关未保存时，顶部切换语音开关不会冲掉未保存草稿
  const jevPrev = builtinJevForm({ jev: { model: "typesafe/jev-1.14", voice_enabled: false, feedback_enabled: false } });
  const jevEdited = { ...jevPrev, api_key: "sk-or-draft", feedback_enabled: true };
  const jevNext = builtinJevForm({ jev: { model: "typesafe/jev-1.13", voice_enabled: true, feedback_enabled: false } });
  assert.deepEqual(syncJevDraft(jevPrev, jevNext, jevEdited), {
    api_key: "sk-or-draft",
    model: "typesafe/jev-1.13",
    voice_enabled: true,
    feedback_enabled: true,
  });
});

test("MOSS 全息语音孪生与全局机械眼：三链路自适应且绝不伪造音频振幅或泄漏密钥", async () => {
  const { buildTwinWaveBars, globalEyeSensorView, mossEyeCorePalette, mossEyeTwinView } = await import("../src/voiceState.js");

  // 1. 内置服务链路：随真实 60ms 音频振幅驱动声波，记录识别字数、已播句数与工具名
  const builtinIdle = mossEyeTwinView({
    mode: "builtin",
    builtinStatus: { running: false, sessions: [], events: [] },
  });
  assert.equal(builtinIdle.visualState, "offline");
  assert.equal(builtinIdle.sourceBadge, "REALTIME AUDIO TWIN");
  assert.equal(builtinIdle.level, 0);
  assert.equal(builtinIdle.bars.length, 12);

  const builtinSpeaking = mossEyeTwinView({
    mode: "builtin",
    builtinStatus: {
      running: true,
      mode: "chat",
      sessions: [
        { session_id: "s1", state: "idle", level: 0 },
        {
          session_id: "s2",
          state: "speaking",
          level: 78,
          utterance_chars: 12,
          spoken_sentences: 3,
          active_tool: "laser_workflow_tool",
          emotion: "happy",
        },
      ],
      events: [{ at: 1, text: "首句延迟：420 ms（说完到首句音频就绪）" }],
    },
  });
  assert.equal(builtinSpeaking.visualState, "speaking");
  assert.equal(builtinSpeaking.level, 78);
  assert.equal(builtinSpeaking.activePolling, true);
  assert.ok(builtinSpeaking.bars[5] > builtinSpeaking.bars[0], "中央声波柱按对称权重高于边缘");
  assert.deepEqual(builtinSpeaking.bars, buildTwinWaveBars("speaking", 78));
  assert.equal(builtinSpeaking.metrics.find((m) => m.label === "实时振幅").value, "78%");
  assert.equal(builtinSpeaking.metrics.find((m) => m.label === "本轮识别").value, "12 字");
  assert.equal(builtinSpeaking.metrics.find((m) => m.label === "播报/路由").value, "已播 3 句");

  // 2. 自建服务链路：进程日志自动脱敏，无音频流时 level 恒为 0，工具调用触发 pulse
  const selfHosted = mossEyeTwinView({
    mode: "self_hosted",
    status: {
      server: { state: "running", pid: 4242 },
      bridge: { connected: true },
      endpoint: { health: { robot_connections: 2 } },
    },
    mcpStatus: { connected: true, last_request: "tools/call: material_params_tool" },
    logLines: ["[info] connected api_key=sk-secret-value-123"],
  });
  assert.equal(selfHosted.visualState, "pulse");
  assert.equal(selfHosted.sourceBadge, "LOCAL SERVER BRIDGE");
  assert.equal(selfHosted.level, 0, "自建链路不伪造音频振幅");
  assert.equal(selfHosted.subline.includes("sk-secret-value-123"), false, "进程日志必须脱敏");
  assert.match(selfHosted.headline, /material_params_tool/);

  // 3. 官方云端链路：同步云端 MCP 握手、消息帧数、最近工具调用与审批警戒
  const official = mossEyeTwinView({
    mode: "official",
    status: { mcp_endpoint: "wss://api.example.test/mcp/?token=***", bridge: { connected: true } },
    mcpStatus: {
      configured: true,
      connected: true,
      messages_count: 19,
      last_request: "tools/call: get_status_tool",
      last_request_time: "14:20:08",
      tools_count: 19,
    },
  });
  assert.equal(official.visualState, "pulse");
  assert.equal(official.sourceBadge, "CLOUD MCP TELEMETRY");
  assert.equal(official.level, 0);
  assert.equal(official.metrics.find((m) => m.label === "指令帧数").value, "19 帧");

  // 4. 全局左侧栏机械眼（globalEyeSensorView）：跨页面跟随语音、MCP、待审批数、激光硬件握手与雕刻加工任务
  assert.equal(
    globalEyeSensorView({ voiceStatus: { mode: "builtin", phase: "listening", level: 64, device_sessions: 1 } }).visualState,
    "listening",
  );
  assert.equal(
    globalEyeSensorView({ voiceStatus: { mode: "builtin", phase: "speaking", level: 85, device_sessions: 1 }, approvalCount: 2 }).visualState,
    "alert",
    "待审批告警优先提示",
  );
  const hwOnlineEye = globalEyeSensorView({
    voiceStatus: { mode: "official" },
    mcpStatus: { connected: false },
    connectionStatus: { connected: true, checkedAt: 1700000000000 },
  });
  assert.equal(hwOnlineEye.hardwareState, "online", "硬件握手通过后外环点亮 online");
  assert.equal(hwOnlineEye.visualState, "idle", "即使语音未连接，硬件就绪时也不显示死寂 offline");
  assert.equal(hwOnlineEye.shortBadge, "硬件已连接");
  assert.match(hwOnlineEye.label, /激光硬件已就绪/);

  const offlineEye = globalEyeSensorView({
    voiceStatus: { mode: "official" },
    mcpStatus: { connected: false },
  });
  assert.equal(offlineEye.visualState, "offline");
  assert.equal(offlineEye.shortBadge, "未连接 · 待机");

  const engravingEye = globalEyeSensorView({
    voiceStatus: { mode: "builtin", device_sessions: 1 },
    connectionStatus: { connected: true, checkedAt: 1700000000000 },
    activeJobs: [{ jobId: "job-1", percent: 62, unknown: false }],
  });
  assert.equal(engravingEye.visualState, "engraving", "激光雕刻进行中切换为 engraving 激光态");
  assert.equal(engravingEye.hardwareState, "engraving");
  assert.equal(engravingEye.jobProgress, 62);
  assert.equal(engravingEye.shortBadge, "正在发送 62%");
  assert.match(engravingEye.label, /激光加工中 62%/);

  const engravingWhileSpeaking = globalEyeSensorView({
    voiceStatus: { mode: "builtin", phase: "speaking", level: 74, device_sessions: 1 },
    activeJobs: [{ jobId: "job-1", percent: 45, unknown: false }],
  });
  assert.equal(engravingWhileSpeaking.visualState, "speaking", "边雕刻边说话时内核跟随语音播报振幅");
  assert.equal(engravingWhileSpeaking.hardwareState, "engraving", "边雕刻边说话时外环同步保持激光加工高速旋转");
  assert.equal(engravingWhileSpeaking.shortBadge, "播报中 · 正在发送 45%");

  const uncertainJobEye = globalEyeSensorView({
    voiceStatus: { mode: "official" },
    activeJobs: [{ jobId: "job-2", unknown: true }],
  });
  assert.equal(uncertainJobEye.visualState, "alert", "连接异常的未知状态加工任务触发 alert 警戒");
  assert.equal(uncertainJobEye.hardwareState, "fault");
  assert.equal(uncertainJobEye.shortBadge, "加工连接异常");

  // 5. 异常数值（Infinity / NaN / 负数）与急停事件短语脱敏边界
  const edgeBuiltin = mossEyeTwinView({
    mode: "builtin",
    approvalCount: Infinity,
    builtinStatus: {
      running: true,
      sessions: [{ session_id: "s1", state: "listening", level: NaN, utterance_chars: -5, spoken_sentences: Infinity }],
      events: [{ at: 2, text: "流式中间结果整句命中急停词（马上停下）：立即请求急停" }],
    },
  });
  assert.equal(edgeBuiltin.approvalAlert, false, "Infinity 不得当作有效审批数");
  assert.equal(edgeBuiltin.level, 0);
  assert.equal(edgeBuiltin.subline.includes("马上停下"), false, "急停事件括号内口语文本不回显到全息舱");
  assert.equal(edgeBuiltin.metrics.find((m) => m.label === "本轮识别").value, "—");

  // 6. 多态球核色盘与节奏映射（mossEyeCorePalette）：各状态核心渐变与未知状态回退
  assert.equal(mossEyeCorePalette("listening").tempo, "acoustic");
  assert.equal(mossEyeCorePalette("listening").accent, "#38bdf8");
  assert.equal(mossEyeCorePalette("thinking").tempo, "neural");
  assert.equal(mossEyeCorePalette("alert").tempo, "alert");
  assert.equal(mossEyeCorePalette("offline").tempo, "dormant");
  assert.equal(mossEyeCorePalette("unknown_state").tempo, "cruise");
  assert.equal(mossEyeCorePalette("speaking").coreStops.length, 5);
  assert.equal(mossEyeCorePalette("engraving").tempo, "laser");
  assert.equal(mossEyeCorePalette("engraving").accent, "#10b981");
});

test("本地 Laya 激活时保存 Jev 卡片开关或备用密钥，不会把本地路由覆写成云端模型", () => {
  const localStatus = {
    jev: {
      model: "laya_multilingual",
      key_set: true,
      local_installed: true,
      voice_enabled: true,
      feedback_enabled: false,
    },
    local: {
      settings: { jev_model: "laya_multilingual", jev_filter_enabled: true },
      models: [{ id: "laya_multilingual", kind: "jev", installed: true }],
    },
  };
  const baseline = builtinJevForm(localStatus);
  assert.equal(baseline.model, "typesafe/jev-1.14", "表单云端输入框显示默认备用模型");

  // 仅打开加工反馈开关或录入备用 OpenRouter 密钥：保持 laya_multilingual
  const toggledFeedback = builtinJevSavePayload(localStatus, { ...baseline, feedback_enabled: true }, baseline);
  assert.equal(toggledFeedback.model, "laya_multilingual");
  assert.equal(toggledFeedback.feedback_enabled, true);

  const backupKey = builtinJevSavePayload(localStatus, { ...baseline, api_key: "sk-or-v1-backup" }, baseline);
  assert.equal(backupKey.model, "laya_multilingual");
  assert.equal(backupKey.api_key, "sk-or-v1-backup");

  // 用户在表单中显式改选了其他云端模型：按用户所选模型提交
  const switchedCloud = builtinJevSavePayload(localStatus, { ...baseline, model: "typesafe/jev-1.13" }, baseline);
  assert.equal(switchedCloud.model, "typesafe/jev-1.13");

  // 若本地 Laya 权重已被删除但路由仍设为 laya_multilingual（且本机存有旧备用云密钥），仅改开关仍保持 laya_multilingual 而不静默切回云端外发
  const uninstalledStatus = {
    ...localStatus,
    jev: { ...localStatus.jev, local_installed: false },
    local: { ...localStatus.local, models: [{ id: "laya_multilingual", kind: "jev", installed: false }] },
  };
  const stillLocal = builtinJevSavePayload(uninstalledStatus, { ...baseline, feedback_enabled: true }, baseline);
  assert.equal(stillLocal.model, "laya_multilingual");

  // 若本地权重已卸载且用户显式手填了新的云端 API Key，则按表单所选云端模型提交，避免被已删除的本地模型卡死
  const switchDeletedToCloud = builtinJevSavePayload(
    uninstalledStatus,
    { ...baseline, api_key: "sk-or-v1-new-cloud" },
    baseline,
  );
  assert.equal(switchDeletedToCloud.model, "typesafe/jev-1.14");
  assert.equal(switchDeletedToCloud.api_key, "sk-or-v1-new-cloud");
});

test("OneJev 1.6 (0.8B / 4B / 9B) 与 Laya 本地决策模型共存：规范化、按模型独立检查安装态、顶部下拉与备用密钥互不覆盖", async () => {
  const {
    builtinJevForm,
    builtinJevSavePayload,
    builtinJevSummary,
    builtinSectionSourceView,
    builtinSourceSwitchPatch,
    builtinSourceSwitchPayload,
    canonicalLocalJevModel,
    isLocalJevModel,
    isLocalLayaJevModel,
    isLocalOneJevModel,
    isSpecificLocalJevInstalled,
  } = await import("../src/voiceState.js");

  assert.equal(canonicalLocalJevModel("onejev_1_6_0_8b"), "onejev_1_6_0_8b");
  assert.equal(canonicalLocalJevModel("OmniJev/OneJev1.6-0.8B"), "onejev_1_6_0_8b");
  assert.equal(canonicalLocalJevModel("onejev-1.6-4b"), "onejev_1_6_4b");
  assert.equal(canonicalLocalJevModel("OmniJev/OneJev1.6-9B"), "onejev_1_6_9b");
  assert.equal(canonicalLocalJevModel("laya/multilingual"), "laya_multilingual");
  assert.equal(canonicalLocalJevModel("typesafe/jev-1.14"), "");

  for (const id of ["onejev_1_6_0_8b", "onejev_1_6_4b", "onejev_1_6_9b"]) {
    assert.equal(isLocalOneJevModel(id), true, id);
    assert.equal(isLocalLayaJevModel(id), false, id);
    assert.equal(isLocalJevModel(id), true, id);
  }
  assert.equal(isLocalOneJevModel("laya_multilingual"), false);
  assert.equal(isLocalLayaJevModel("laya_multilingual"), true);
  assert.equal(isLocalJevModel("laya_multilingual"), true);

  const catalogModels = [
    { id: "onejev_1_6_0_8b", kind: "jev", label: "OneJev 1.6 0.8B（轻量极速，推荐）", installed: true },
    { id: "onejev_1_6_4b", kind: "jev", label: "OneJev 1.6 4B（均衡主力）", installed: true },
    { id: "onejev_1_6_9b", kind: "jev", label: "OneJev 1.6 9B（高阶旗舰）", installed: false },
    { id: "laya_multilingual", kind: "jev", label: "Laya 决策模型（322M 多语种）", installed: true },
  ];
  const baseStatus = {
    jev: {
      model: "onejev_1_6_4b",
      key_set: true,
      local_installed: true,
      voice_enabled: true,
      feedback_enabled: false,
    },
    local: {
      jev_key_configured: true,
      settings: { jev_filter_enabled: true, jev_model: "onejev_1_6_4b" },
      models: catalogModels,
    },
  };

  assert.equal(isSpecificLocalJevInstalled(baseStatus, "onejev_1_6_0_8b"), true);
  assert.equal(isSpecificLocalJevInstalled(baseStatus, "onejev_1_6_4b"), true);
  assert.equal(isSpecificLocalJevInstalled(baseStatus, "onejev_1_6_9b"), false, "未下载的 9B 不因其他本地 Jev 已装而误判");
  assert.equal(isSpecificLocalJevInstalled(baseStatus, "laya_multilingual"), true);

  const view4b = builtinSectionSourceView("jev", baseStatus);
  assert.equal(view4b.source, "local");
  assert.equal(view4b.localOn, true);
  assert.equal(view4b.localReady, true);
  assert.equal(view4b.selectValue, "local:onejev_1_6_4b");
  assert.equal(view4b.activeText, "本地（OneJev 1.6 4B（均衡主力））");
  assert.deepEqual(
    view4b.options.filter((opt) => opt.isLocal).map((opt) => [opt.value, opt.installed]),
    [
      ["local:onejev_1_6_0_8b", true],
      ["local:onejev_1_6_4b", true],
      ["local:onejev_1_6_9b", false],
      ["local:laya_multilingual", true],
    ],
  );
  assert.equal(builtinJevSummary(baseStatus), "使用本地模型（云端设置作备用）");

  // 顶部下拉在 OneJev 0.8B / 4B / 9B 与 Laya 之间自由切换
  for (const id of ["onejev_1_6_0_8b", "onejev_1_6_4b", "onejev_1_6_9b", "laya_multilingual"]) {
    assert.deepEqual(builtinSourceSwitchPatch(baseStatus, "jev", `local:${id}`), {
      jev_filter_enabled: true,
      jev_model: id,
    });
    const full = builtinSourceSwitchPayload(baseStatus, "jev", `local:${id}`);
    assert.equal(full.jev_filter_enabled, true);
    assert.equal(full.jev_model, id);
  }

  // OneJev 4B 激活时在云端 Jev 卡片打开加工反馈或保存备用密钥，保持 onejev_1_6_4b 不被覆写为云端模型
  const baseline = builtinJevForm(baseStatus);
  assert.equal(baseline.model, "typesafe/jev-1.14");
  const savedFeedback = builtinJevSavePayload(baseStatus, { ...baseline, feedback_enabled: true }, baseline);
  assert.equal(savedFeedback.model, "onejev_1_6_4b");
  assert.equal(savedFeedback.feedback_enabled, true);
  const savedBackupKey = builtinJevSavePayload(baseStatus, { ...baseline, api_key: "sk-or-v1-backup" }, baseline);
  assert.equal(savedBackupKey.model, "onejev_1_6_4b");

  // 若当前路由选在未下载的 9B（哪怕 Laya 已装），localReady 为 false；此时手填云端密钥可顺利切回云端
  const uninstalled9bStatus = {
    ...baseStatus,
    jev: { ...baseStatus.jev, model: "onejev_1_6_9b", local_installed: false },
    local: { ...baseStatus.local, settings: { jev_filter_enabled: true, jev_model: "onejev_1_6_9b" } },
  };
  const view9b = builtinSectionSourceView("jev", uninstalled9bStatus);
  assert.equal(view9b.localReady, false);
  assert.match(view9b.activeText, /未下载/);
  const switchedFromUninstalled9b = builtinJevSavePayload(
    uninstalled9bStatus,
    { ...baseline, api_key: "sk-or-v1-cloud" },
    baseline,
  );
  assert.equal(switchedFromUninstalled9b.model, "typesafe/jev-1.14");
});

test("n59：内置 MOSS 角色、自建角色的校验与保存载荷", () => {
  const base = localSettingsForm();
  const ids = personaOptions(base.custom_personas).map((opt) => opt.id);
  assert.deepEqual(ids, ["workshop", "beginner_guide", "moss_station", "moss_briefing", "moss_risk", "moss_deadpan", "moss_night"]);
  for (const id of ids) assert.equal(localSettingsError({ ...base, persona_style: id }), "", id);
  assert.match(localSettingsError({ ...base, persona_style: "custom_a1" }), /说话角色/);

  const roles = [{ id: "custom_a1", name: " 老师傅 ", prompt: " 话少，先说结论 " }];
  const form = localSettingsForm({ custom_personas: roles, persona_style: "custom_a1" });
  assert.equal(form.custom_personas, JSON.stringify(roles), "form keeps the list as text for dirty checks");
  assert.equal(localSettingsError(form), "");
  assert.equal(personaOptions(form.custom_personas).at(-1).label, "老师傅（自建）");
  assert.deepEqual(localSettingsPayload(form).custom_personas, [{ id: "custom_a1", name: "老师傅", prompt: "话少，先说结论" }]);
  assert.equal(localSettingsPayload(form).persona_style, "custom_a1");

  const withRoles = (list) => ({ ...base, custom_personas: JSON.stringify(list) });
  assert.match(localSettingsError(withRoles([{ id: "custom_a1", name: "", prompt: "话少" }])), /名字/);
  assert.match(localSettingsError(withRoles([{ id: "custom_a1", name: "老师傅", prompt: "" }])), /人设/);
  assert.match(localSettingsError(withRoles([{ id: "custom_a1", name: "老师傅", prompt: "收到就发 G1 X10" }])), /G-code/);
  assert.match(localSettingsError(withRoles([{ id: "custom_a1", name: "甲", prompt: "a" }, { id: "custom_a2", name: "甲", prompt: "b" }])), /读起来一样/);
  assert.match(localSettingsError(withRoles([{ id: "bad", name: "甲", prompt: "a" }])), /编号/);
  const seven = Array.from({ length: MAX_CUSTOM_PERSONAS + 1 }, (_, i) => ({ id: `custom_${i}`, name: `角色${i}`, prompt: "话少" }));
  assert.match(localSettingsError(withRoles(seven)), /最多/);
  assert.match(localSettingsError(withRoles([{ id: "custom_a1", name: "Foo", prompt: "a" }, { id: "custom_a2", name: "foo", prompt: "b" }])), /读起来一样/);
  assert.match(localSettingsError(withRoles([{ id: "custom_a1", name: "！！！", prompt: "a" }])), /文字或数字/);
  assert.match(localSettingsError(withRoles([{ id: "custom_a1", name: "甲", prompt: "a" }, { id: "custom_a1", name: "乙", prompt: "b" }])), /编号重复/);
  assert.match(localSettingsError({ ...base, custom_personas: "[broken" }), /损坏/, "broken form data never becomes an empty list");
  for (const compact of ["G1X10", "G01X10Y20", "M3S1000", "G28"]) {
    assert.match(localSettingsError(withRoles([{ id: "custom_a1", name: "老师傅", prompt: compact }])), /G-code/, compact);
  }
  for (const fine of ["M30 螺丝", "G10 是坐标设置", "G90 绝对坐标"]) {
    assert.equal(localSettingsError(withRoles([{ id: "custom_a1", name: "老师傅", prompt: fine }])), "", fine);
  }
  assert.match(newCustomPersonaId(roles), /^custom_[a-z0-9]{1,16}$/);
  assert.deepEqual(parseCustomPersonas("not json"), []);
});

test("n51：助手风格安全校验、同音错词规则与小智硬件在线快捷控制视图", () => {
  const base = localSettingsForm();
  assert.equal(base.persona_style, "workshop");
  assert.equal(base.asr_homophone_fix, true);
  assert.equal(base.screen_preview_sync, true);
  assert.equal(base.screen_card_style, "moss_hud");

  const station = {
    ...base,
    persona_style: "moss_station",
    persona_prompt: "  翼宿 5W 激光工坊  ",
    custom_asr_corrections: " 段木 = 椴木 \n一宿->翼宿\n段木=东北椴木",
  };
  assert.equal(localSettingsError(station), "");
  const payload = localSettingsPayload(station);
  assert.equal(payload.persona_style, "moss_station");
  assert.equal(payload.persona_prompt, "翼宿 5W 激光工坊");
  assert.deepEqual(payload.custom_asr_corrections, ["段木=东北椴木", "一宿=翼宿"]);
  assert.equal(localSettingsPayload({ ...station, screen_card_style: "goggle_bot" }).screen_card_style, "goggle_bot");
  assert.match(localSettingsError({ ...base, screen_card_style: "invalid_style" }), /画风/);

  assert.match(localSettingsError({ ...base, persona_prompt: "执行 G1 X20 Y20" }), /G-code/);
  assert.match(localSettingsError({ ...base, custom_asr_corrections: "急停=继续" }), /安全保护词/);
  assert.match(localSettingsError({ ...base, custom_asr_corrections: "测试=G0 X10" }), /G-code|控制器/);
  assert.equal(parseAsrCorrectionLine("亚克立→亚克力").formatted, "亚克立=亚克力");

  const ctrl = builtinDeviceControlsView(
    {
      device_id: "aa:bb:cc",
      status: "allowed",
      status_json: JSON.stringify({ audio_speaker: { volume: 0 }, screen: { brightness: 45, theme: "dark" } }),
      listen_json: JSON.stringify({ mode: "manual" }),
    },
    {
      session_id: "s1",
      device_id: "aa:bb:cc",
      screen_monochrome: false,
      screen_width: 320,
      screen_height: 240,
      device_tools: [
        "self.audio_speaker.set_volume",
        "self.screen.set_brightness",
        "self.screen.set_theme",
        "self.screen.preview_image",
        "self.set_press_to_talk",
      ],
    },
    { "aa:bb:cc": { volume: 0, brightness: 45, theme: "dark" } }
  );
  assert.equal(ctrl.online, true);
  assert.equal(ctrl.canVolume, true);
  assert.equal(ctrl.canBrightness, true);
  assert.equal(ctrl.canTheme, true);
  assert.equal(ctrl.canPressToTalk, true);
  assert.equal(ctrl.canPreviewImage, true);
  assert.equal(ctrl.screenResolution, "320×240");
  assert.equal(ctrl.talkMode, "press_to_talk");
  assert.equal(ctrl.volume, 0);
  assert.equal(ctrl.brightness, 45);
  assert.equal(ctrl.theme, "dark");
});

test("n61 wake word drafts are checked before saving", async () => {
  const { WAKE_WORD_PRESETS, wakeWordDraftError } = await import("../src/voiceState.js");
  assert.equal(WAKE_WORD_PRESETS[0].command, "ni hao mo si");
  for (const preset of WAKE_WORD_PRESETS) assert.equal(wakeWordDraftError({ ...preset, threshold: 0.2 }), "");
  assert.match(wakeWordDraftError({ text: "", command: "mo si", threshold: 0.2 }), /填写/);
  assert.match(wakeWordDraftError({ text: "字".repeat(17), command: "mo si", threshold: 0.2 }), /16/);
  assert.match(wakeWordDraftError({ text: "MOSS", command: "moss", threshold: 0.2 }), /2～6/);
  assert.match(wakeWordDraftError({ text: "MOSS", command: "mó sī", threshold: 0.2 }), /声调/);
  assert.match(wakeWordDraftError({ text: "MOSS", command: "mo si", threshold: 1 }), /阈值/);
});

test("本地 Jev 运行状态：失败带原因、加载中说明会跳过、9B / CPU 提示随状态带出", () => {
  assert.equal(localJevRuntimeView(null), null);
  assert.equal(localJevRuntimeView({ state: "cloud" }), null);
  assert.equal(localJevRuntimeView({ state: "not_installed", model: "onejev_1_6_4b" }), null);
  const ready = localJevRuntimeView({ state: "ready", accelerator: "Apple Metal GPU", notice: null });
  assert.equal(ready.tone, "ok");
  assert.match(ready.text, /已就绪（Apple Metal GPU）/);
  const slow = localJevRuntimeView({ state: "ready", accelerator: "CPU", notice: "建议改用 0.8B" });
  assert.equal(slow.tone, "warn");
  assert.equal(slow.detail, "建议改用 0.8B");
  assert.match(localJevRuntimeView({ state: "starting" }).text, /正在加载/);
  const failed = localJevRuntimeView({ state: "failed", error: "OneJev 模型加载超时" });
  assert.equal(failed.tone, "bad");
  assert.match(failed.text, /启动失败：OneJev 模型加载超时/);
  assert.match(localJevRuntimeView({ state: "failed" }).text, /未知原因/);
  assert.match(localJevRuntimeView({ state: "stopped" }).text, /未在运行/);
  assert.equal(localJevRuntimeView({ state: "idle" }), null);
  assert.equal(localJevRuntimeView({ state: "idle", notice: "9B 已关闭没说完判断" }).tone, "off");
});

test("测试全部环节：按每个环节当前选用的来源决定怎么测，不通的给出下一步（2026-11-08）", async () => {
  const { pipelineTestPlan, pipelineTestOutcome, pipelineTestSummary } = await import("../src/voiceState.js");
  const base = {
    models: {
      llm: { configured: true, url: "https://llm.test/v1", model: "qwen-max", key_set: true },
      asr: { configured: true, url: "https://asr.test/v1/audio/transcriptions", model: "SenseVoiceSmall", key_set: true },
      tts: { configured: true, url: "https://tts.test/v1/audio/speech", model: "MOSS/TTS-1", voice: "alloy", speed: 1, key_set: true },
    },
    jev: { key_set: false, model: "typesafe/jev-1.14" },
    local: {
      helper_found: true,
      settings: { vad: false, asr: false, tts: false, vad_model: "silero_vad", asr_model: "sense_voice", tts_model: "kokoro", jev_filter_enabled: false },
      models: [
        { id: "silero_vad", kind: "vad", label: "Silero VAD", installed: true },
        { id: "sense_voice", kind: "asr", label: "SenseVoice 2024", installed: true },
        { id: "kokoro", kind: "tts", label: "Kokoro v1.0", installed: false },
        { id: "onejev_1_6_0_8b", kind: "jev", label: "OneJev 1.6 0.8B", installed: true },
      ],
    },
  };
  const byKey = (plan) => Object.fromEntries(plan.map((step) => [step.key, step]));

  // 全部云端 + 能量判停 + Jev 关闭：三个云端环节真实请求，载荷来自已保存配置且密钥留空
  let plan = byKey(pipelineTestPlan(base));
  assert.deepEqual(Object.keys(plan), ["vad", "asr", "jev", "llm", "tts"], "顺序与链路栏一致");
  assert.equal(plan.vad.kind, "static");
  assert.equal(plan.vad.ok, true);
  assert.equal(plan.jev.skipped, true);
  assert.equal(plan.llm.kind, "section");
  assert.deepEqual(plan.llm.input, { url: "https://llm.test/v1", api_key: "", model: "qwen-max" });
  assert.equal(plan.tts.input.voice, "alloy");

  // 本地识别已下载 → 就绪；本地合成未下载 → 报错并给出下一步；本地判停已下载 → 就绪
  const local = structuredClone(base);
  Object.assign(local.local.settings, { vad: true, asr: true, tts: true });
  plan = byKey(pipelineTestPlan(local));
  assert.equal(plan.vad.ok, true);
  assert.match(plan.asr.detail, /本地 SenseVoice 2024 已下载/);
  assert.equal(plan.tts.ok, false);
  assert.match(plan.tts.detail, /Kokoro v1\.0.*还没下载/);
  // 找不到本地模型程序：已下载的本地环节也判不通
  local.local.helper_found = false;
  plan = byKey(pipelineTestPlan(local));
  assert.equal(plan.asr.ok, false);
  assert.match(plan.asr.detail, /moss-voice-local/);

  // Jev：云端未配密钥 → 报错；配了密钥 → 用已保存密钥测已选模型；本地已下载 → 测本地模型
  const jevCloud = structuredClone(base);
  jevCloud.local.settings.jev_filter_enabled = true;
  jevCloud.local.settings.jev_model = "typesafe/jev-1.13";
  plan = byKey(pipelineTestPlan(jevCloud));
  assert.equal(plan.jev.ok, false);
  assert.match(plan.jev.detail, /OpenRouter API Key/);
  jevCloud.jev.key_set = true;
  plan = byKey(pipelineTestPlan(jevCloud));
  assert.deepEqual(plan.jev, { key: "jev", kind: "jev", input: { api_key: "", model: "typesafe/jev-1.13" } });
  jevCloud.local.settings.jev_model = "onejev_1_6_0_8b";
  plan = byKey(pipelineTestPlan(jevCloud));
  assert.deepEqual(plan.jev.input, { model: "onejev_1_6_0_8b" });

  // 云端未配置或已保存配置缺字段：不发请求，直接报错
  const missing = structuredClone(base);
  delete missing.models.asr;
  missing.models.llm.model = "";
  plan = byKey(pipelineTestPlan(missing));
  assert.equal(plan.asr.ok, false);
  assert.match(plan.asr.detail, /云端语音识别还没配置.*或在上方改用本地模型/);
  assert.equal(plan.llm.kind, "static");
  assert.match(plan.llm.detail, /已保存的配置不完整：请填写模型名/);

  // 结论：通过带耗时，失败保留服务方原因，调用异常走错误文案；汇总点名不通的环节
  assert.deepEqual(pipelineTestOutcome({ ok: true, elapsed_ms: 320, detail: "回复正常" }), { ok: true, detail: "回复正常（320 ms）" });
  assert.deepEqual(pipelineTestOutcome({ ok: false, elapsed_ms: 90, detail: "HTTP 401 密钥无效" }), { ok: false, detail: "HTTP 401 密钥无效" });
  assert.deepEqual(pipelineTestOutcome(null, new Error("网络不通"), (e) => `出错：${e.message}`), { ok: false, detail: "出错：网络不通" });
  assert.deepEqual(
    pipelineTestSummary({ vad: { ok: true }, asr: { ok: false }, jev: { ok: true, skipped: true }, llm: { ok: true }, tts: { ok: false } }),
    { ok: false, text: "2 个环节不通：语音识别、语音合成。原因见对应下拉框下方" },
  );
  assert.deepEqual(
    pipelineTestSummary({ vad: { ok: true }, asr: { ok: true }, jev: { ok: true, skipped: true }, llm: { ok: true }, tts: { ok: true } }),
    { ok: true, text: "全部环节测试通过（4 项正常，其余已关闭跳过）" },
  );
});
