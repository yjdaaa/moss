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

test("NV8a 复制诊断仅保留允许字段，不复制对话、音频、凭据和地址", () => {
  const sensitive = "never-copy-this-value";
  const item = {
    turn: 3, outcome: "interrupted", models: ["fake-asr"],
    transcript: sensitive, audio: sensitive, authorization: sensitive, url: sensitive, device_id: sensitive,
    stages: { recording_start: 0, barge_in: 430, first_audio_frame: 750, unknown: sensitive, final_asr: Infinity },
    tools: Array.from({ length: 70 }, () => ({ name: "fake_status", ms: 15, arguments: sensitive, result: sensitive })),
  };
  const report = voiceTimingReport(Array(30).fill(item));
  assert.equal(report.length, 20);
  assert.equal(report[0].tools.length, 50);
  assert.deepEqual(report[0].stages, { barge_in: 430, recording_start: 0, first_audio_frame: 750 });
  assert.deepEqual(report[0].tools[0], { name: "fake_status", ms: 15 });
  assert.equal(JSON.stringify(report).includes(sensitive), false);
  assert.deepEqual(voiceTimingReport(null), []);
});
import {
  approvalHistoryStatus,
  canSaveSection,
  deviceView,
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
  selectLocalModel,
  selectedVoices,
  voiceTimingReport,
  localSettingsForm,
  localSettingsError,
  localSettingsPayload,
  endpointLabel,
  endpointView,
  identityView,
  toolSwitchPayload,
  toolStatText,
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
  savedNoteText,
  dirtyTabs,
  builtinModelSummary,
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
    read_only_shortcuts: true, asr_beam_search: true, custom_hotwords: "",
    barge_in: false, barge_in_ms: "400",
    tts_speaker: "3", tts_speed: "1.2", mirror: "",
  });
  assert.equal(localSettingsError(form), "");
  assert.match(localSettingsError({ ...form, tts_speaker: "1.5" }), /整数/);
  assert.match(localSettingsError({ ...form, tts_speed: "3" }), /0.5/);
  assert.match(localSettingsError({ ...form, mirror: "ftp://x" }), /http/);
  assert.deepEqual(localSettingsPayload({ ...form, mirror: " https://m.test/x " }), {
    asr: true, tts: false, vad: false, vad_model: "silero_vad", asr_model: "sense_voice", tts_model: "kokoro",
    read_only_shortcuts: true, asr_beam_search: true, custom_hotwords: [],
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
