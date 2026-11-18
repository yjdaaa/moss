// 语音中心设置：接入点与工具开关、标签页、设置分组的脏检查与保存载荷。

import { builtinModeView } from "./devices.js";
import { localSettingsPayload } from "./localModels.js";

// ---------- NV8 接入点与工具 ----------

const ENDPOINT_LABELS = { primary: "主接入点" };

/** 接入点编号的中文名：primary → 主接入点，extra-2 → 附加接入点 2。 */
export function endpointLabel(id) {
  if (ENDPOINT_LABELS[id]) return ENDPOINT_LABELS[id];
  const match = /^extra-(\d+)$/.exec(String(id || ""));
  return match ? `附加接入点 ${match[1]}` : String(id || "接入点");
}

/** 一个接入点的状态显示：`{tone, text, detail}`。 */
export function endpointView(endpoint) {
  if (!endpoint) return { tone: "off", text: "未配置", detail: "" };
  const reconnects = Math.max(0, Number(endpoint.connections || 0) - 1);
  if (endpoint.connected) {
    const parts = [];
    if (endpoint.client) parts.push(`对方：${endpoint.client}`);
    if (reconnects > 0) parts.push(`已自动重连 ${reconnects} 次`);
    return { tone: "ok", text: "已连接", detail: parts.join(" · ") };
  }
  if (endpoint.last_error) return { tone: "bad", text: "重连中", detail: `上次断开：${endpoint.last_error}` };
  return { tone: "warn", text: "连接中", detail: "" };
}

/** 接入点 token 里的身份：`{text, expired}`；不是 JWT 时 text 为空。 */
export function identityView(identity, nowSeconds = Date.now() / 1000) {
  if (!identity || typeof identity !== "object") return { text: "", expired: false };
  const fields = identity.fields && typeof identity.fields === "object" ? identity.fields : {};
  const parts = Object.entries(fields).map(([key, value]) => `${key} ${value}`);
  const exp = Number(identity.expires_at);
  let expired = false;
  if (Number.isFinite(exp) && exp > 0) {
    expired = exp < nowSeconds;
    const date = new Date(exp * 1000).toLocaleDateString("zh-CN");
    parts.push(expired ? `已于 ${date} 过期` : `有效期至 ${date}`);
  }
  return { text: parts.join(" · "), expired };
}

/** 切换一个工具后的“关闭名单”（排序、去重）；不能关闭的工具不会进入名单。 */
export function toolSwitchPayload(tools, name, enabled) {
  const list = Array.isArray(tools) ? tools : [];
  const disabled = new Set(list.filter((tool) => tool && tool.enabled === false && tool.can_disable !== false).map((tool) => tool.name));
  const target = list.find((tool) => tool?.name === name);
  if (target && target.can_disable !== false) {
    if (enabled) disabled.delete(name);
    else disabled.add(name);
  }
  return [...disabled].sort();
}

/**
 * n72（2026-11-08 用户决定）：内置工具卡片给人看的名字、分组和风险标签。
 * `risk`：read 只读 / file 只生成预览或文件 / confirm 会让机器动、要你确认 / stop 停止与调速（不用确认）/ desktop 控制电脑。
 * 精简清单（默认给 AI）以后端 `in_compact` 为准；这里只管显示。
 */
export const TOOL_GROUPS = [
  { id: "safety", title: "检查与安全" },
  { id: "job", title: "刻字与发送" },
  { id: "tuning", title: "材料与调参" },
  { id: "advanced", title: "高级：重复入口与底层收发（只在“完整工具”打开时给 AI）" },
  { id: "desktop", title: "电脑控制（内置语音和严格模式下不开放）" },
  { id: "other", title: "其他" },
];

export const TOOL_RISK_LABELS = {
  read: "只读",
  file: "只生成预览",
  stop: "停止与调速",
  confirm: "会让机器动 · 要你确认",
  desktop: "控制电脑",
};

export const TOOL_META = {
  check_laser_connection_tool: { label: "检查机器连接", group: "safety", risk: "read", summary: "看激光机连没连上、现在空闲还是在刻。" },
  laser_safe_action_tool: { label: "停止与机器控制", group: "safety", risk: "confirm", summary: "急停、边刻边调速度和功率不用确认；回零、设原点、移动激光头要你确认。不能关闭。" },
  laser_workflow_tool: { label: "刻字：预览、发送、进度", group: "job", risk: "confirm", summary: "先生成预览弹到电脑上，你核对并确认后才开始刻；也能查进度、取消、断点续刻。" },
  regenerate_text_laser_task_tool: { label: "按建议重新生成预览", group: "job", risk: "file", summary: "采用调参建议或改文字、材料后重新出预览；发送仍要你确认。" },
  material_params_tool: { label: "材料参数库", group: "tuning", risk: "read", summary: "查某种材料该用多大功率、多快速度；保存和删除只能在电脑上做。" },
  refine_laser_params_from_feedback_tool: { label: "按效果给调参建议", group: "tuning", risk: "file", summary: "你说“太焦”“太浅”后给出调整建议，不改参数、不动机器。" },
  run_calibration_grid_tool: { label: "测试矩阵", group: "tuning", risk: "confirm", summary: "刻一块不同功率×速度的小方格找最佳参数；开始刻前要你确认。" },
  select_calibration_cell_tool: { label: "保存最佳格子", group: "tuning", risk: "file", summary: "你说第几格最好，就把它的参数存进材料库。" },
  recommend_laser_params_tool: { label: "推荐参数（旧入口）", group: "advanced", risk: "read", summary: "和“材料参数库”的查询重复。" },
  start_tuned_job_tool: { label: "按调参结果开刻（旧入口）", group: "advanced", risk: "confirm", summary: "和“刻字”的确认发送重复。" },
  generate_text_laser_task_tool: { label: "生成刻字任务（旧入口）", group: "advanced", risk: "file", summary: "和“刻字”的预览重复。" },
  generate_text_image_gcode_tool: { label: "文字转刀路文件", group: "advanced", risk: "file", summary: "只生成图片和刀路文件。" },
  ai_laser_gcode_tool: { label: "素材转刀路（旧入口）", group: "advanced", risk: "file", summary: "和“刻字”的预览重复。" },
  generate_text_image_tool: { label: "生成文字图片", group: "advanced", risk: "file", summary: "只生成白底黑字图片。" },
  laser_grbl_tool: { label: "串口底层收发", group: "advanced", risk: "confirm", summary: "直接对串口查状态、取消；发任意 G-code 已禁用。" },
  laser_network_grbl_tool: { label: "网络底层收发", group: "advanced", risk: "confirm", summary: "直接对网络雕刻机查状态、取消；发任意 G-code 已禁用。" },
  open_app_tool: { label: "打开电脑应用", group: "desktop", risk: "desktop", summary: "打开浏览器、微信等应用或网址。" },
  shortcut_key_execution_tool: { label: "电脑快捷键", group: "desktop", risk: "desktop", summary: "播放、暂停音乐，调电脑音量。" },
  open_jsjds_website_tool: { label: "打开计算机设计大赛官网", group: "desktop", risk: "desktop", summary: "打开大赛官网。" },
  command_execution_tool: { label: "执行终端命令（开发者）", group: "desktop", risk: "desktop", summary: "只在开发者模式出现。" },
};

/** 工具按分组排好，未登记的工具放进“其他”，用后端给的说明。 */
export function groupedTools(tools) {
  const list = Array.isArray(tools) ? tools : [];
  return TOOL_GROUPS.map((group) => ({
    ...group,
    tools: list
      .map((tool) => {
        const meta = TOOL_META[tool.name];
        return {
          ...tool,
          label: meta?.label || tool.name,
          group: meta?.group || "other",
          risk: meta?.risk || "",
          summary: meta?.summary || tool.description || "",
        };
      })
      .filter((tool) => tool.group === group.id),
  })).filter((group) => group.tools.length > 0);
}

/** 工具调用统计的简短文字。 */
export function toolStatText(tool) {
  const calls = Number(tool?.calls || 0);
  if (!calls) return "本次运行未调用";
  const parts = [`调用 ${calls} 次`];
  if (tool.failures) parts.push(`失败 ${tool.failures}`);
  if (tool.denied) parts.push(`拒绝 ${tool.denied}`);
  return parts.join(" · ");
}

/**
 * 内置工具（编译在 MOSS 里的原生工具）不能在界面上增删，只能开关；
 * 这两段是交给 AI 编码助手的施工说明，用户复制后粘给 AI 由它改代码（2026-11-02 用户决定）。
 * 增删第三方外部工具走「扩展 → 第三方 MCP 工具」，与这里无关。
 */
export const TOOL_EDIT_PROMPTS = {
  add: {
    title: "让 AI 新增一个内置工具",
    text: [
      "请在 MOSS 仓库里新增一个内置工具：<工具名>，用途是 <一句话说明它做什么>。",
      "",
      "按下面顺序改，只动相关文件，不要顺手重构：",
      "1. 登记：crates/laser-tools/src/lib.rs 的 PRIORITY_TOOLS / SECONDARY_TOOLS 名单里加入新工具名（snake_case、以 _tool 结尾，不含双下划线 __，与现有工具一致）；ToolCatalog 会自动把它收进名单。",
      "2. 声明：tests/golden/mcp_tools.json（被 crates/laser-mcp/src/lib.rs 的 tools_list_json / tools_list_with 编译期加载）增加它的 name、description 与 inputSchema（JSON Schema，必填项与取值范围写清楚）。默认给 AI 的是精简清单：若这个工具也要默认提供，同时把面向用户意图写的短说明加进 tests/golden/mcp_tools_compact.json、名字加进 laser_mcp::COMPACT_LASER_TOOLS 与 laser_api::laser_mcp_compact_tool，并在 frontend/src/voiceState.js 的 TOOL_META 里写中文名、分组与风险标签。",
      "3. 分发与分级：crates/laser-mcp/src/lib.rs 的 dispatch_tool_call_with 增加同名 match 分支（非法参数返回结构化错误，不 panic），并在 crates/laser-contracts/src/risk.rs 的 tool_risk 显式标定 R0–R4 风险等级。",
      "4. 实现：复用现有的连接解析 resolve_connection_spec、发送门禁 authorize_send 与错误处理，不新增抽象层。",
      "5. 门禁：涉及运动、出光、发送 G-code、急停、删除、OTA、密钥的动作必须走现有审批与严格模式门禁，不得放宽；只读工具不得有副作用；急停类工具必须 can_disable=false。",
      "6. Jev 粗筛归组：在 crates/moss-voice-server/src/jev.rs 的 groups_of_builtin_tool 将新工具归入现有 6 个粗粒度 ToolGroup（LaserQuery / LaserGenerate / LaserControl / InfoPlugin / HomeAndAgent / MusicAndDevice，跨类激光工具可挂多组）；严禁为单个新工具平铺独立的 noul 问题。",
      "7. 测试：在 crates/laser-mcp、crates/laser-tools 与 crates/moss-voice-server 增加单元测试（参数校验、关闭后不可见、Jev 归组、失败路径），硬件与网络副作用一律用 mock 或 FakeGrbl。",
      "8. 记录：同步 docs/developer/api-contracts.md 与对应技术书，在 CHANGELOG.md 的 [Unreleased] 加一条；跑 cargo test --workspace。",
      "",
      "完成后回报：新工具名、用途、风险等级与所属粗筛组、是否需要审批、改了哪些文件、跑了哪些测试及结果。",
    ].join("\n"),
  },
  remove: {
    title: "让 AI 删除或下线一个内置工具",
    text: [
      "请把 MOSS 内置工具里的 <工具名> 下线或删除。",
      "",
      "先判断用哪种做法：",
      "1. 只是不想让模型用到它：不用改代码。在「扩展 → MOSS 内置工具」里取消勾选即可，关闭的工具不会出现在任何接入点与内置语音的工具列表中。",
      "2. 要彻底删掉代码：按下面顺序执行。",
      "",
      "彻底删除的顺序：",
      "1. 从 crates/laser-tools/src/lib.rs 的 PRIORITY_TOOLS / SECONDARY_TOOLS 名单移除。",
      "2. 从 tests/golden/mcp_tools.json、tests/golden/mcp_tools_compact.json（若在精简清单里，同时从 COMPACT_LASER_TOOLS 与 laser_api::laser_mcp_compact_tool 去掉）、crates/laser-mcp/src/lib.rs（tools_list_with / dispatch_tool_call_with）及 crates/laser-contracts/src/risk.rs（tool_risk）移除对应 schema、分发与分级分支，并删掉 frontend/src/voiceState.js 的 TOOL_META 条目。",
      "3. 删除该工具的实现、只属于它的测试，并从 crates/moss-voice-server/src/jev.rs 的 groups_of_builtin_tool 清理该工具名（Jev 按当轮启用工具动态提问，删空某组也无需改 ToolGroup 枚举）。",
      "4. 在 crates/、frontend/、docs/、scripts/ 全局搜索残留引用并逐条清理，把清单告诉我。",
      "",
      "不要动：laser_safe_action_tool（急停与停机）、严格模式、审批与 ACL 门禁；crates/laser-host/src/lib.rs 的 validate_disabled_tools 中不可关闭的约束必须保留。",
      "",
      "完成后：跑 cargo test --workspace 与前端 npm test，同步 docs/developer/api-contracts.md 与 CHANGELOG.md，并回报实际做法、删除清单、残留引用检查结果与测试结果。",
    ].join("\n"),
  },
};

/** 取指定模板的纯文本；类型未知时返回空字符串。 */
export function toolEditPrompt(kind) {
  return TOOL_EDIT_PROMPTS[kind]?.text || "";
}

/** 新增附加接入点前的校验；通过时返回空字符串。 */
export function extraEndpointError(url, endpoints = [], max = 4) {
  const value = String(url || "").trim();
  if (!value) return "请填写接入点地址";
  if (!/^wss?:\/\//.test(value) || value.includes("<") || /\s/.test(value)) return "接入点必须以 ws:// 或 wss:// 开头，且不含占位符";
  const extras = (Array.isArray(endpoints) ? endpoints : []).filter((endpoint) => String(endpoint?.id || "").startsWith("extra-"));
  if (extras.length >= max) return `附加接入点最多 ${max} 个`;
  return "";
}

// ---------- 语音中心分组与保存（UI-3，技术书 §9.5.1 r15） ----------

/**
 * 页面标签。**当前链路用不到的部分不渲染**（2026-10-18 用户决定）；切标签不会丢失未保存的填写。
 * n69（2026-11-07 用户决定“完整整理”）：“唤醒词与音量”并入“设备”，“设备与工具”改名“设备”，内置工具移到“扩展”。
 */
export const VOICE_TABS = [
  { key: "service", label: "服务" },
  { key: "devices", label: "设备" },
  { key: "models", label: "模型" },
  { key: "features", label: "扩展" },
  { key: "privacy", label: "隐私与家居" },
  { key: "records", label: "记录与诊断" },
];

/** 已取消的旧标签 → 新位置（记住的标签或跳转目标仍指向旧标签时使用）。 */
const LEGACY_VOICE_TABS = { wake: "devices" };

/** 把记住的 / 外部传来的标签 key 换成现有标签；不认识的返回空串。 */
export function normalizeVoiceTab(key) {
  const value = LEGACY_VOICE_TABS[key] || key;
  return VOICE_TABS.some((item) => item.key === value) ? value : "";
}

/**
 * 当前链路要显示的标签（2026-10-18 / 2026-11-02 用户决定）：
 * - 官方链路的模型在小智控制台配置，不显示“模型”标签（2026-11-17 删除自建链路后，“模型”只属于内置链路）；
 * - “隐私与家居”四项（对话记忆、信息服务、声纹、家居）均挂在 MOSS 内置语音会话上，只在 `builtin` 链路显示。
 */
export function voiceTabsFor(mode) {
  return VOICE_TABS.filter((item) => {
    if (item.key === "models" && mode !== "builtin") return false;
    if (item.key === "privacy" && mode !== "builtin") return false;
    return true;
  });
}

/**
 * 右上角偏好设置是否显示“OpenRouter · Jev 判断服务”（加工反馈判断，`/api/jev`）。
 * 桌面端只有内置链路的“模型 → Jev”卡片能配这项；官方链路没有“模型”标签，
 * 所以除内置链路外都在偏好设置里保留入口（2026-11-15 用户决定）。没有语音中心的网页环境一直显示；
 * 链路还没读到（null）时先不显示，避免内置链路下闪一下。
 */
export function preferencesShowJev({ hasVoiceCenter, voiceMode }) {
  if (!hasVoiceCenter) return true;
  return voiceMode != null && voiceMode !== "builtin";
}

/** 语音链路名称（2026-11-17 删除自建服务端链路，只剩这两条）。 */
export const VOICE_MODE_LABEL = { official: "官方小智", builtin: "MOSS 内置服务" };

/**
 * 链路与服务端设置拆成独立保存的分组：每组只提交自己的字段（后端按字段合并，见 center.rs save_settings）。
 * `secret` 字段带令牌，不回填；填了才算改动，留空表示不改。
 */
export const SETTINGS_GROUPS = {
  official: { mode: "official", title: "官方接入点", keys: ["official_mcp_endpoint"], secret: ["official_mcp_endpoint"] },
  builtinPorts: { mode: "builtin", title: "内置服务端口", keys: ["builtin_ws_port", "builtin_http_port"], secret: [] },
  // n71（2026-11-08）：常连模式（MQTT + 加密 UDP），默认关
  builtinMqtt: { mode: "builtin", title: "常连模式（实验）", keys: ["builtin_mqtt_enabled", "builtin_mqtt_port", "builtin_udp_port"], secret: [] },
};

const PORT_KEYS = new Set(["builtin_ws_port", "builtin_http_port", "builtin_mqtt_port", "builtin_udp_port"]);
const BOOL_KEYS = new Set(["builtin_mqtt_enabled"]);

/** 表单值与已保存值逐项比较（都按去首尾空白的字符串比较）；`keys` 缺省为两边字段的并集。 */
export function formDirty(baseline, form, keys) {
  if (!baseline || !form) return false;
  return (keys || [...new Set([...Object.keys(baseline), ...Object.keys(form)])]).some((key) => String(form[key] ?? "").trim() !== String(baseline[key] ?? "").trim());
}

/**
 * 设置分组保存后按钮旁的生效说明。`reconnected` 取自后端返回（center.rs save_settings），
 * 只在真的重新发起了连接时才说“正在重新连接”；是否连上看页面顶部的连接状态。
 */
export function settingsSaveEffect(group, reconnected) {
  if (group === "builtinPorts") return "重启内置服务后生效";
  if (group === "builtinMqtt") return "重启内置服务后生效；小智要再重启一次才会换连接方式";
  return reconnected ? "正在重新连接小智" : "接入点未变，没有重新连接";
}

/** 某个设置分组是否有未保存的改动。 */
export function settingsGroupDirty(group, form, baseline) {
  const spec = SETTINGS_GROUPS[group];
  if (!spec || !form) return false;
  const plain = spec.keys.filter((key) => !spec.secret.includes(key));
  return spec.secret.some((key) => String(form[key] || "").trim() !== "") || formDirty(baseline, form, plain);
}

/** 某个设置分组的保存内容：只含本组字段，端口转为数字，带令牌的字段留空时不提交。 */
export function settingsGroupPayload(group, form) {
  const spec = SETTINGS_GROUPS[group];
  const payload = {};
  for (const key of spec.keys) {
    const value = String(form[key] ?? "").trim();
    if (spec.secret.includes(key)) {
      if (value) payload[key] = value;
    } else if (BOOL_KEYS.has(key)) {
      payload[key] = value === "true";
    } else if (PORT_KEYS.has(key)) {
      const port = Number(value);
      payload[key] = Number.isFinite(port) ? port : 0;
    } else {
      payload[key] = value;
    }
  }
  return payload;
}

/**
 * 本地模型设置拆成两组分别保存：模型选择与对话体验。
 * n69（2026-11-07）：`screen_card_style` 只在“设备 › 副屏预览”点选即存，`jev_filter_enabled` 只由“模型”标签顶部第 3 栏切换，
 * 两者都不再属于对话体验组的草稿与保存载荷（避免两处编辑同一个键）。
 */
export const LOCAL_MODEL_KEYS = ["vad", "vad_model", "denoise", "denoise_model", "kws", "kws_model", "turn", "turn_model", "asr", "asr_model", "tts", "tts_model", "tts_speaker", "tts_speed", "mirror"];
export const EXPERIENCE_KEYS = [
  "read_only_shortcuts",
  "asr_beam_search",
  "job_done_announce",
  "ignore_bystanders",
  "llm_fast_reply",
  "screen_preview_sync",
  "persona_style",
  "persona_prompt",
  "custom_personas",
  "persona_risk_notes",
  "asr_homophone_fix",
  "custom_asr_corrections",
  "jev_unfinished_hold",
  "jev_fast_route",
  "jev_tool_prune",
  "jev_memory_filter",
  "barge_in",
  "barge_in_ms",
  "custom_hotwords",
];

/** 只取草稿中本组字段，其余用已保存的值，避免保存一组时顺带提交另一组未保存的改动。 */
export function mergeGroup(baseline, draft, keys) {
  const next = { ...baseline };
  for (const key of keys) next[key] = draft[key];
  return next;
}

/**
 * 外部渠道（如顶部 5 栏下拉框或 Jev 模型卡片）保存后刷新 baseline 时：
 * 仅保留用户在当前分组草稿中相对上一次 baseline 真实手动改动过的字段，其余未改动字段自动跟随新 baseline。
 */
export function syncLocalDraft(prevBaseline, nextBaseline, currentDraft, keys) {
  const prev = prevBaseline || nextBaseline;
  const next = { ...nextBaseline };
  for (const key of keys) {
    if (currentDraft && currentDraft[key] !== prev[key]) {
      next[key] = currentDraft[key];
    }
  }
  return next;
}

/** 仅提取本分组相对当前已保存 baseline 真正改动的键值作为局部保存载荷，防止跨组旧草稿覆盖外部刚切换的开关。 */
export function localGroupPatch(baseline, draft, keys) {
  const basePayload = localSettingsPayload(baseline);
  const mergedPayload = localSettingsPayload(mergeGroup(baseline, draft, keys));
  const patch = {};
  for (const key of keys) {
    if (JSON.stringify(mergedPayload[key]) !== JSON.stringify(basePayload[key])) {
      patch[key] = mergedPayload[key];
    }
  }
  return Object.keys(patch).length > 0 ? patch : Object.fromEntries(keys.map((k) => [k, mergedPayload[k]]));
}

function clock(at) {
  return at.toLocaleTimeString("zh-CN", { hour12: false });
}

/** 保存成功后按钮旁的一行结果：保存了什么、几点、什么时候生效。 */
export function savedNoteText(target, effect, at = new Date()) {
  return [`已保存${target}`, clock(at), effect].filter(Boolean).join(" · ");
}

/** 保存成功后按钮旁的生效说明：后端热应用即写“已立即生效”，否则按是否重启给出提示。 */
export function restartEffect(next) {
  if (next?.hot_applied) return "已立即生效，无需重启";
  const mode = builtinModeView(next);
  return next?.restarted ? `内置服务已重启（${mode.text}模式）` : `下次启动内置服务时生效（${mode.text}模式）`;
}

/** 有未保存改动的分组所在的标签（显示标签上的提示点）。`dirty` 为 {分组 id: 标签 key}。 */
export function dirtyTabs(dirty) {
  return new Set(Object.values(dirty || {}).filter(Boolean));
}
