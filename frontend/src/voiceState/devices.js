// 小智设备：设备信息与授权、在线快捷控制、屏幕画风、声纹标定、固件推送与唤醒词。

const DEVICE_STATUS = {
  pending: { text: "待允许", tone: "warn" },
  allowed: { text: "已允许", tone: "ok" },
  revoked: { text: "已撤销", tone: "off" },
};

/**
 * 设备信息快照（schema 14）：把设备开机上报（info）、连接握手（hello）、实时状态（status）
 * 三段原始 JSON 摊成可显示的行。字段随固件版本变化，缺什么就显示「—」，不猜。
 */
export function builtinDeviceInfoView(device) {
  const parse = (text) => {
    if (!text) return null;
    try {
      const value = JSON.parse(text);
      return value && typeof value === "object" ? value : null;
    } catch {
      return null;
    }
  };
  const text = (value) => (value === undefined || value === null || value === "" ? "—" : String(value));
  const size = (bytes) => {
    const number = Number(bytes);
    if (!Number.isFinite(number) || number <= 0) return "—";
    return number >= 1024 * 1024 ? `${Math.round(number / 1024 / 1024)} MB` : `${Math.round(number / 1024)} KB`;
  };
  const info = parse(device?.info_json);
  const hello = parse(device?.hello_json);
  const status = parse(device?.status_json);
  const app = info?.application || {};
  const audio = hello?.audio;
  const rows = [
    ["固件版本", text(app.version || device?.firmware_version)],
    ["应用名", text(app.name)],
    ["芯片", text(info?.chip_model_name)],
    ["Flash / PSRAM", `${size(info?.flash_size)} / ${size(info?.psram_size)}`],
    ["服务端 AEC 声明", typeof hello?.aec === "boolean" ? (hello.aec ? "已声明" : "未声明") : "未上报"],
    ["设备端回声消除", "固件不上报，看实际拾音模式（realtime 即已开启），本面板不推断"],
    [
      "实际拾音模式",
      (() => {
        const mode = parse(device?.listen_json)?.mode;
        if (mode === "realtime") return "realtime（固件选了实时对话，说明设备端 AEC 开启）";
        if (mode === "auto") return "auto（设备端 AEC 关闭或未启用）";
        if (mode === "manual") return "manual（手动说话，无法判断 AEC）";
        return "未上报";
      })(),
    ],
    ["帧协议", hello ? `v${text(hello.protocol_version)} · ${text(hello.transport)}` : "—"],
    ["上行音频", audio ? `${text(audio.sample_rate)} Hz · ${text(audio.format)} · ${text(audio.frame_duration)} ms` : "—"],
    ["设备自带 MCP", hello ? (hello.mcp ? "支持" : "不支持") : "—"],
    ["界面语言", text(info?.language)],
    ["IDF 版本", text(app.idf_version)],
    ["编译时间", text(app.compile_time)],
    ["MAC", text(info?.mac_address)],
    ["UUID", text(info?.uuid)],
  ];
  return {
    rows,
    hasInfo: Boolean(info),
    hasHello: Boolean(hello),
    hasStatus: Boolean(status),
    status: status || null,
    updatedAt: Number(device?.info_at) || 0,
  };
}

export const SCREEN_CARD_STYLE_OPTIONS = [
  {
    id: "moss_hud",
    label: "硬核太空舱（MOSS HUD）",
    shortLabel: "硬核太空舱",
    hint: "直角瞄准刻度、深空暗底与 MOSS 红色光学眼",
  },
  {
    id: "goggle_bot",
    label: "可爱像素小机器人（Goggle Bot）",
    shortLabel: "可爱像素小机器人",
    hint: "星空暗底、小 MOSS 护目镜像素小机器人陪你聊天、雕刻、审批、听歌",
  },
];

// n70（2026-11-08）：小智全屏画风 12 个场景，顺序与 `moss-voice-server::screen_pack::SCENES` 一致。
export const GOGGLE_BOT_POSES = [
  { id: "idle", label: "待命眨眼", desc: "护目镜架额头眨眼；固件待命时显示", mossLabel: "太空舱待命红眼", mossDesc: "红色光学眼慢呼吸；固件待命时显示", botLabel: "待命眨眼", botDesc: "护目镜架额头眨眼；固件待命时显示" },
  { id: "listen", label: "竖耳聆听", desc: "耳罩亮起，手拢在耳边", mossLabel: "太空舱声波聆听", mossDesc: "青色声波圈一圈圈扩散", botLabel: "竖耳聆听", botDesc: "耳罩亮起，手拢在耳边" },
  { id: "think", label: "托腮思考", desc: "眼睛往右上看，冒思考泡泡", mossLabel: "太空舱扫描思考", mossDesc: "琥珀扫描环转圈", botLabel: "托腮思考", botDesc: "眼睛往右上看，冒思考泡泡" },
  { id: "talk", label: "张嘴说话", desc: "嘴巴一张一合，小手比划", mossLabel: "太空舱波形说话", mossDesc: "两侧声波柱跟着跳", botLabel: "张嘴说话", botDesc: "嘴巴一张一合，小手比划" },
  { id: "happy", label: "开心举手", desc: "开心类表情：月牙笑眼、双手举起", mossLabel: "太空舱青色愉悦", mossDesc: "开心类表情：青色光眼 + 上升箭头", botLabel: "开心举手", botDesc: "开心类表情：月牙笑眼、双手举起" },
  { id: "sad", label: "难过掉泪", desc: "难过类表情：耷拉眼、小乌云下雨", mossLabel: "太空舱蓝色低落", mossDesc: "难过类表情：蓝色光眼 + 雨丝", botLabel: "难过掉泪", botDesc: "难过类表情：耷拉眼、小乌云下雨" },
  { id: "surprised", label: "惊讶圆眼", desc: "惊讶类表情：圆眼 O 形嘴、感叹号", mossLabel: "太空舱白光惊讶", mossDesc: "惊讶类表情：白光眼 + 闪光环", botLabel: "惊讶圆眼", botDesc: "惊讶类表情：圆眼 O 形嘴、感叹号" },
  { id: "engrave", label: "戴护目镜雕刻", desc: "开始雕刻后显示；有刀路时中央叠刀路与进度", mossLabel: "太空舱雕刻表盘", mossDesc: "开始雕刻后显示；有刀路时中央叠刀路与进度", botLabel: "戴护目镜雕刻", botDesc: "开始雕刻后显示；有刀路时中央叠刀路与进度" },
  { id: "approval", label: "待电脑审批提醒", desc: "举牌挥手，倒计时写在字幕", mossLabel: "太空舱审批警戒门禁", mossDesc: "琥珀边框闪烁，倒计时写在字幕", botLabel: "待电脑审批提醒", botDesc: "举牌挥手，倒计时写在字幕" },
  { id: "music", label: "听歌摇摆", desc: "戴耳机摇摆、音符飘起，停播为止", mossLabel: "太空舱黑胶像素唱机", mossDesc: "唱盘旋转、音符飘起，停播为止", botLabel: "听歌摇摆", botDesc: "戴耳机摇摆、音符飘起，停播为止" },
  { id: "celebrate", label: "完工欢呼", desc: "竖大拇指、彩纸落下，停留约 10 秒", mossLabel: "太空舱完工徽章", mossDesc: "青色光芒旋转，停留约 10 秒", botLabel: "完工欢呼", botDesc: "竖大拇指、彩纸落下，停留约 10 秒" },
  { id: "halted", label: "急停护盾", desc: "戴好护目镜举盾，停留约 10 秒", mossLabel: "太空舱安全急停", mossDesc: "红框警示角闪烁，停留约 10 秒", botLabel: "急停护盾", botDesc: "戴好护目镜举盾，停留约 10 秒" },
];

/** 计算单台小智设备的在线快捷控制能力与当前显示值（结合实时快照、会话工具列表与本机记忆偏好）。 */
export function builtinDeviceControlsView(device, session, devicePrefs = {}) {
  let statusObj = null;
  try {
    statusObj = device?.status_json ? JSON.parse(device.status_json) : null;
  } catch {
    statusObj = null;
  }
  let helloObj = null;
  try {
    helloObj = device?.hello_json ? JSON.parse(device.hello_json) : null;
  } catch {
    helloObj = null;
  }
  let listenObj = null;
  let parsedListen = null;
  try {
    parsedListen = device?.listen_json ? JSON.parse(device.listen_json) : null;
  } catch {
    parsedListen = null;
  }
  void listenObj;
  const pref = (devicePrefs && device?.device_id && devicePrefs[device.device_id]) || {};
  const tools = Array.isArray(session?.device_tools) ? session.device_tools : [];
  const hasToolList = tools.length > 0;
  const mcpSupported = Boolean(session) && (hasToolList || helloObj?.mcp !== false);
  const canVolume = hasToolList
    ? tools.includes("self.audio_speaker.set_volume")
    : Boolean(mcpSupported);
  const canBrightness = hasToolList
    ? tools.includes("self.screen.set_brightness") || tools.includes("self.disp.setbacklight")
    : Boolean(statusObj?.screen && statusObj.screen.brightness !== undefined);
  const canTheme = hasToolList
    ? tools.includes("self.screen.set_theme")
    : Boolean(statusObj?.screen && statusObj.screen.theme);
  const canPressToTalk = hasToolList
    ? tools.includes("self.set_press_to_talk")
    : false;
  const screenMonochrome =
    session?.screen_monochrome === true
      ? true
      : session?.screen_monochrome === false
        ? false
        : null;
  const screenWidth = Number(session?.screen_width);
  const screenHeight = Number(session?.screen_height);
  const screenResolution =
    Number.isFinite(screenWidth) && screenWidth > 0 && Number.isFinite(screenHeight) && screenHeight > 0
      ? `${Math.round(screenWidth)}×${Math.round(screenHeight)}`
      : null;
  const canPreviewImage = hasToolList
    ? tools.includes("self.screen.preview_image") && screenMonochrome !== true
    : false;
  const clampPct = (val, fallback) => {
    const num = Number(val);
    if (!Number.isFinite(num)) return fallback;
    return Math.max(0, Math.min(100, Math.round(num)));
  };
  const liveVol = statusObj?.audio_speaker?.volume;
  const liveBri = statusObj?.screen?.brightness;
  const liveTheme = statusObj?.screen?.theme;
  const volume = liveVol !== undefined ? clampPct(liveVol, 60) : clampPct(pref.volume, 60);
  const brightness = liveBri !== undefined ? clampPct(liveBri, 80) : clampPct(pref.brightness, 80);
  const theme = liveTheme === "light" || liveTheme === "dark"
    ? liveTheme
    : pref.theme === "light" || pref.theme === "dark"
      ? pref.theme
      : "dark";
  const talkMode =
    pref.talk_mode === "press_to_talk" || pref.talk_mode === "click_to_talk"
      ? pref.talk_mode
      : parsedListen?.mode === "manual"
        ? "press_to_talk"
        : "click_to_talk";

  const onlineHasScreen =
    canBrightness
    || canTheme
    || tools.includes("self.screen.preview_image")
    || screenMonochrome === true
    || Boolean(screenResolution);
  const offlineNoMcp = !session && helloObj?.mcp === false;
  // n130 + n132：在线已报工具列表时隐藏设备不支持的控制项（含无屏设备的投屏按钮）；
  // 离线设备仅在 hello 明确声明 mcp=false 时隐藏，其余照 n130 置灰保留（避免单色 OLED / AMOLED 因 status_json 无 screen 键误判无屏）
  const hideMissing = Boolean(session && hasToolList);
  return {
    online: Boolean(session),
    mcpSupported,
    showVolume: hideMissing ? canVolume : !offlineNoMcp,
    showBrightness: hideMissing ? canBrightness : !offlineNoMcp,
    showTheme: hideMissing ? canTheme : !offlineNoMcp,
    showPreview: hideMissing ? onlineHasScreen : !offlineNoMcp,
    showPressToTalk: hideMissing ? canPressToTalk : !offlineNoMcp,
    canVolume,
    canBrightness,
    canTheme,
    canPressToTalk,
    canPreviewImage,
    screenMonochrome,
    screenResolution,
    volume,
    brightness,
    theme,
    talkMode,
    savedPref: {
      volume: pref.volume ?? null,
      brightness: pref.brightness ?? null,
      theme: pref.theme || "",
      talk_mode: pref.talk_mode || "",
    },
  };
}

/**
 * 是否应展示「副屏预览（小智彩屏画风）」卡片：
 * - 尚未绑定任何设备时保留展示（方便预先浏览选画风）；
 * - 已有未撤销设备时，若所有设备均已确认是单色 OLED 或无屏设备，则隐藏彩屏专属画风卡。
 */
export function hasColorScreenPreviewTarget(status) {
  const devices = Array.isArray(status?.devices) ? status.devices.filter((d) => d && d.status !== "revoked") : [];
  if (devices.length === 0) return true;
  const sessions = Array.isArray(status?.sessions) ? status.sessions : [];
  const isKnownNonColor = (device) => {
    const sess = sessions.find((s) => s.device_id === device.device_id);
    if (sess) {
      if (sess.screen_monochrome === true) return true;
      const tools = Array.isArray(sess.device_tools) ? sess.device_tools : [];
      if (tools.length > 0) {
        const hasColorPreview = tools.includes("self.screen.preview_image");
        const hasColorSize = Number(sess.screen_width) > 0 && Number(sess.screen_height) > 0 && sess.screen_monochrome === false;
        return !hasColorPreview && !hasColorSize;
      }
      return false;
    }
    try {
      const helloObj = device?.hello_json ? JSON.parse(device.hello_json) : null;
      if (helloObj?.mcp === false) return true;
    } catch {
      // 忽略损坏快照
    }
    return false;
  };
  return devices.some((d) => !isKnownNonColor(d));
}
/** 内置语音服务的设备状态与可用操作（内置语音技术书 §6：新设备首次连接需在电脑上允许）。 */
export function builtinDeviceView(device) {
  const status = device?.status || "pending";
  const base = DEVICE_STATUS[status] || DEVICE_STATUS.pending;
  return {
    ...base,
    status,
    title: (device?.name || "").trim() || device?.device_id || "未知设备",
    canAllow: status !== "allowed",
    canRevoke: status !== "revoked",
  };
}

/** 允许设备前的确认文字。 */
export function allowDeviceConfirmText(device) {
  const facts = [
    `设备：${builtinDeviceView(device).title}`,
    device?.model ? `型号：${device.model}` : "",
    device?.firmware_version ? `固件：${device.firmware_version}` : "",
  ].filter(Boolean);
  return [
    ...facts,
    "",
    "允许后，这台设备可以通过 MOSS 的内置语音服务对话，并永久记住。请确认是你自己的设备。",
    "允许后直接唤醒设备即可对话（常连模式需重启设备生效）。",
  ].join("\n");
}

/** 内置语音服务当前（或下次启动）的工作模式（NV5）。 */
export function builtinModeView(status) {
  const chat = status?.mode === "chat";
  const linked = status?.link_mode === "builtin";
  return {
    text: chat ? "对话" : "回声测试",
    tone: chat ? "ok" : "info",
    hint: chat
      ? "已配置识别、大模型与合成：设备说话后由 MOSS 识别、回答并朗读。开始加工前会先复述参数等你口头确认；开启严格模式时还需在电脑上批准。"
      : "识别、大模型、合成三项没有配置齐全：先以回声测试运行（说完稍停即把你的声音播回）。识别、合成也可用本地模型；在“模型”标签配齐后自动切换为对话。",
    autostart: linked
      ? "语音链路为“MOSS 内置服务”：随 MOSS 自动启动。"
      : "语音链路切到“MOSS 内置服务”后会随 MOSS 自动启动；现在也可以手动启动测试。",
  };
}

// ---------- 声纹简易标定（2026-10-14 用户决定，取代“源码写死不开放”） ----------

/** 标定区的显示：状态文字、每人测试音条数、能否计算阈值。 */
export function speakerCalibrationView(data) {
  const speakers = Array.isArray(data?.speakers) ? data.speakers : [];
  const clips = data?.test_clips && typeof data.test_clips === "object" ? data.test_clips : {};
  const calibration = data?.calibration || { state: "none" };
  const report = calibration.report;
  const result = report
    ? `阈值 ${Number(report.threshold).toFixed(2)} · 本人认对 ${report.genuine_accepted}/${report.genuine_trials} · 认成别人 ${report.false_accepts}/${report.impostor_trials}`
    : "";
  const status = {
    valid: `已标定（${report?.people ?? 0} 人）：${result}`,
    stale: "登记名单在标定后变过（有人新增、删除或重新登记），需要重新标定；在此之前不识别身份",
    none: "未标定：身份识别与按人记忆保持关闭",
  }[calibration.state] || "未标定：身份识别与按人记忆保持关闭";
  const counts = Object.fromEntries(speakers.map((row) => [row.id, Number(clips[row.id]) || 0]));
  const ready = speakers.length >= 2 && speakers.every((row) => counts[row.id] >= 3);
  const blocked = speakers.length < 2 ? "至少需要两个已登记的人" : ready ? "" : "每个人都要录满三段测试音";
  return { state: calibration.state || "none", status, counts, ready, blocked };
}

// ---------- 固件 OTA 推送记录（2026-10-15） ----------

/** 推送记录的显示：只有设备上报了新版本才算升级完成；下载开始不等于刷写成功。 */
export function firmwareDeliveryView(row, now = Date.now() / 1000) {
  if (row?.device_report_status === "reported") return { text: "已升级（设备已上报新版本）", cancellable: false };
  const started = row?.download_status === "downloading" || row?.download_status === "unknown";
  if (!(Number(row?.expires_at) > Number(now))) {
    return { text: started ? "推送已失效；设备曾开始下载，请核对设备当前版本" : "推送已失效（已取消或已过期）", cancellable: false };
  }
  if (row.download_status === "pending") return { text: "等待设备检查更新（重启设备即可）", cancellable: true };
  if (row.download_status === "downloading") return { text: "设备已开始下载，等待上报新版本", cancellable: true };
  return { text: "下载结果未知（下载时 MOSS 重启过），等待设备上报版本", cancellable: true };
}

// n61 设备唤醒词（2026-11-06）：预设与前端校验；拼音音节是否被模型认识由后端按模型词表最终核对。
export const WAKE_WORD_PRESETS = [
  { text: "你好 MOSS", command: "ni hao mo si" },
  { text: "MOSS MOSS", command: "mo si mo si" },
  { text: "嘿 MOSS", command: "hei mo si" },
];

/** 拼音命令词的比较形式：小写、去首尾空白、音节间单个空格。 */
export function normalizeWakeCommand(command) {
  return String(command ?? "").trim().toLowerCase().split(/\s+/).filter(Boolean).join(" ");
}

/**
 * 改显示文字（2026-11-23 用户决定）：拼音为空或仍是旧文字自动生成的结果时跟着更新；
 * 用户手动改过拼音就不覆盖。`toPinyin` 未加载好时只改文字。
 */
export function applyWakeText(draft, text, toPinyin) {
  const next = { ...draft, text };
  if (typeof toPinyin !== "function") return next;
  const current = normalizeWakeCommand(draft?.command);
  if (!current || current === toPinyin(draft?.text ?? "")) next.command = toPinyin(text);
  return next;
}

export function wakeWordDraftError(draft) {
  const text = String(draft?.text ?? "").trim();
  if (!text) return "请填写唤醒词";
  if ([...text].length > 16) return "唤醒词最多 16 个字";
  if (/\p{Cc}/u.test(text)) return "唤醒词里不能有控制字符";
  const syllables = String(draft?.command ?? "").trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (syllables.length < 2 || syllables.length > 6) return "拼音需要 2～6 个音节，用空格隔开，例如 ni hao mo si";
  const bad = syllables.find((s) => !/^[a-z]+$/.test(s));
  if (bad) return `“${bad}”不是拼音音节（只写字母、不带声调）`;
  const threshold = Number(draft?.threshold);
  if (!Number.isFinite(threshold) || threshold < 0.05 || threshold > 0.95) return "灵敏度阈值须在 0.05～0.95 之间";
  return "";
}

/**
 * 设备全屏画风包状态（2026-11-09）：以设备整包下载完为准，推送了没下载不算“已刷”。
 * `canQuickPack`：彩屏、支持在线更新资源包、还没装好也没在等下载时，显示“一键刷入全屏画风”。
 */
export function screenPackDeviceView(device) {
  const size = Array.isArray(device?.screen_pack_size) ? device.screen_pack_size : null;
  const state = device?.screen_pack_state || (device?.screen_pack ? "installed" : "none");
  const withPack = device?.screen_pack_pending_with_pack !== false;
  let text;
  let tone = "off";
  if (state === "pending_download") {
    text = withPack ? "已推送，等设备下载全屏画风…" : "已推送，等设备下载资源包…";
    tone = "info";
  } else if (state === "download_expired") {
    text = "推送后 30 分钟没下载完：确认设备联网后重新推送";
    tone = "warn";
  } else if (state === "installed") {
    text = "已刷全屏画风，上电就是全屏画面";
    tone = "ok";
  } else if (state === "style_outdated") {
    // 2026-11-09 用户决定：换了画风要重刷，开机待命画面才跟着换
    text = "装的是换画风之前的全屏画风：重刷后开机待命画面才会换";
    tone = "warn";
  } else if (state === "assets_outdated") {
    // 2026-11-13 n84：版本前缀 + 内容指纹对比发现基础底包、字库档位或画面资源与设备当前不一致
    text = "画风或字库资源有更新（底包或字库与设备当前不一致）：重刷后同步到设备";
    tone = "warn";
  } else if (state === "font_outdated") {
    // 2026-11-12 n81：旧版 moss-screen-1 未含常用中文字库，重刷 moss-screen-2 补齐屏幕中文字体
    text = "装的是旧版画风包（缺常用中文字库）：重刷后补齐屏幕中文字体";
    tone = "warn";
  } else if (size) {
    text = `彩屏 ${size[0]}×${size[1]}，还没刷全屏画风`;
    tone = "warn";
  } else {
    text = "没报彩屏分辨率，只能推唤醒词";
  }
  const canQuickPack = Boolean(device?.supported && size && state !== "installed" && state !== "pending_download");
  const quickLabel = state === "font_outdated"
    ? "一键补齐中文字库"
    : state === "assets_outdated"
      ? "一键更新画风字库"
      : state === "style_outdated"
        ? "一键重刷全屏画风"
        : "一键刷入全屏画风";
  return { state, text, tone, canQuickPack, quickLabel };
}
