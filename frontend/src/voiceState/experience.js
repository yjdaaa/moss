// 对话体验：说话角色、自定义角色与同音纠错规则的解析与校验。

/**
 * 内置说话角色（n59；编号与后端 `BUILTIN_PERSONA_IDS` 一致）。角色只改语气，声音由语音合成设置决定。
 * n75（2026-11-08 用户决定）：7 个减为 4 个；“风险评估”改为所有角色可用的开关 `persona_risk_notes`。
 */
export const PERSONA_STYLE_OPTIONS = [
  { id: "workshop", label: "简明工坊助手（默认）", hint: "干练直接，专注激光雕刻指令与状态反馈" },
  { id: "beginner_guide", label: "新手引导", hint: "耗材、对焦、排风、走边框多给一句大白话提醒" },
  { id: "moss_station", label: "MOSS（电影原版）", hint: "像《流浪地球》的 MOSS：收到、先报结论，平稳无起伏，用“提示：”“警告：”开头；没有真实数据就说“数据不足，无法计算”" },
  { id: "moss_deadpan", label: "MOSS 冷面幽默", hint: "正事说完偶尔一句冷幽默；出光、急停、故障时不开玩笑" },
];

/** n75 已合并的旧角色编号 → 现在的编号（与后端 `canonical_persona_style` 一致）。 */
export function canonicalPersonaStyle(id) {
  if (id === "moss_briefing" || id === "moss_risk") return "moss_station";
  if (id === "moss_night") return "workshop";
  return id;
}
export const MAX_CUSTOM_PERSONAS = 6;

/** 自建角色在表单里存为 JSON 文字（便于未保存改动比较），这里读回数组。 */
export function parseCustomPersonas(raw) {
  try {
    const list = typeof raw === "string" ? JSON.parse(raw || "[]") : raw;
    return (Array.isArray(list) ? list : []).map((role) => ({
      id: String(role?.id || ""),
      name: String(role?.name || ""),
      prompt: String(role?.prompt || ""),
    }));
  } catch {
    return [];
  }
}

/** 下拉框里的全部角色：内置在前，自建在后。 */
export function personaOptions(customRaw) {
  const custom = parseCustomPersonas(customRaw)
    .filter((role) => role.id && role.name.trim())
    .map((role) => ({ id: role.id, label: `${role.name.trim()}（自建）`, hint: role.prompt.trim().slice(0, 40) }));
  return [...PERSONA_STYLE_OPTIONS, ...custom];
}

export function newCustomPersonaId(existing = []) {
  const used = new Set(existing.map((role) => role.id));
  for (let i = 1; ; i += 1) {
    const id = `custom_${Date.now().toString(36).slice(-6)}${i}`;
    if (!used.has(id)) return id;
  }
}

/** 语音按名字切换时只看文字和数字、不分大小写（与后端 `persona_name_key` 一致）。 */
function personaNameKey(name) {
  return String(name || "").replace(/[^\p{L}\p{N}]/gu, "").toLowerCase();
}

function customPersonasError(raw) {
  if (typeof raw === "string") {
    try {
      if (!Array.isArray(JSON.parse(raw || "[]"))) return "自建角色数据损坏，请刷新页面";
    } catch {
      return "自建角色数据损坏，请刷新页面";
    }
  }
  const roles = parseCustomPersonas(raw);
  if (roles.length > MAX_CUSTOM_PERSONAS) return `自建角色最多 ${MAX_CUSTOM_PERSONAS} 个`;
  const names = new Set();
  const ids = new Set();
  for (const role of roles) {
    if (!/^custom_[a-z0-9]{1,16}$/.test(role.id)) return "自建角色编号无效，请删除后重新新增";
    if (ids.has(role.id)) return "自建角色编号重复，请删除后重新新增";
    ids.add(role.id);
    const name = role.name.trim();
    if (!name) return "自建角色需要名字";
    if ([...name].length > 12) return "角色名最多 12 个字";
    if (/[\x00-\x1f\x7f]/.test(name) || containsDangerousGcodeOrGrblJs(name)) return "角色名不能包含控制字符或 G-code";
    const key = personaNameKey(name);
    if (!key) return "角色名需要包含文字或数字";
    if (names.has(key)) return `角色名“${name}”和已有角色读起来一样`;
    names.add(key);
    const prompt = role.prompt.replace(/\r\n?/g, "\n").trim();
    if (!prompt) return `角色“${name}”需要写人设`;
    if (/[\x00-\x09\x0b-\x1f\x7f]/.test(prompt)) return "角色人设不能包含控制字符";
    if ([...prompt].length > 300) return "角色人设最多 300 个字符";
    if (containsDangerousGcodeOrGrblJs(prompt)) return "角色人设不能包含 G-code 或控制器 $n= 指令";
  }
  return "";
}

export const BUILTIN_ASR_CORRECTION_PREVIEW =
  "段木/端木→椴木 · 公率/攻率→功率 · 激光鸡→激光机 · 亚克立→亚克力 · 一宿→翼宿 · 牛皮指→牛皮纸 · 胶合版→胶合板 · 走边筐→走边框";

const PROTECTED_STOP_WORDS = new Set([
  "停", "急停", "停止", "立刻停", "马上停", "快停", "取消", "停止加工", "暂停",
  "退出", "再见", "拜拜", "退下", "退一下", "关闭对话", "确认", "开始",
]);

const MOTION_GCODES = ["G0", "G00", "G1", "G01", "G2", "G02", "G3", "G03", "G28", "M3", "M03", "M4", "M04", "M5", "M05"];

function containsDangerousGcodeOrGrblJs(text) {
  const upper = String(text || "").toUpperCase();
  for (const token of upper.split(/[^A-Z0-9$=]+/)) {
    if (!token) continue;
    // Whole word (G1) or compact form followed by a parameter letter (G1X10, M3S1000); mirrors the backend list.
    if (MOTION_GCODES.some((code) => token.startsWith(code) && /^(?:$|[A-Z])/.test(token.slice(code.length)))) return true;
    if (/^\$\d+=/.test(token)) return true;
  }
  return false;
}

export function parseAsrCorrectionLine(rawLine) {
  const line = String(rawLine || "");
  if (/[\x00-\x1f\x7f]/.test(line)) return { error: "同音纠错规则不能包含控制字符" };
  const trimmed = line.trim();
  if (!trimmed) return { empty: true };
  const match = trimmed.match(/^(.*?)(?:=|->|→|＝)(.*)$/);
  if (!match) return { error: "每条同音纠错请按“错词=正词”填写（如 段木=椴木）" };
  const wrong = match[1].trim();
  const right = match[2].trim();
  if (!wrong || !right) return { error: "同音纠错的错词与正词都不能为空" };
  if (wrong === right) return { error: "同音纠错的错词与正词不能相同" };
  if (wrong.includes("=") || right.includes("=")) return { error: "单条同音纠错只能包含一个“=”分隔符" };
  if ([...wrong].length > 24 || [...right].length > 24) return { error: "同音纠错单侧最多 24 个字符" };
  if (PROTECTED_STOP_WORDS.has(wrong.toLowerCase()) || PROTECTED_STOP_WORDS.has(wrong)) {
    return { error: `安全保护词“${wrong}”不允许被同音纠错改写` };
  }
  if (containsDangerousGcodeOrGrblJs(right)) {
    return { error: "同音纠错目标词不能包含 G-code 或控制器 $n= 指令" };
  }
  return { wrong, right, formatted: `${wrong}=${right}` };
}

/** 前端先做的检查，按保存分组拆开；返回错误文字或空串。语音体验分组（插话、热词）： */
export function experienceSettingsError(form) {
  const bargeMs = Number(form.barge_in_ms ?? 400);
  if (!Number.isInteger(bargeMs) || bargeMs < 300 || bargeMs > 600) return "插话时长须为 300 ~ 600 毫秒的整数";
  const words = String(form.custom_hotwords || "").split(/\r?\n/);
  if (words.some((word) => /[\x00-\x1f\x7f]/.test(word))) return "热词不能包含控制字符";
  const effective = [...new Set(words.map((word) => word.trim()).filter(Boolean))];
  if (effective.length > 64) return "自定义热词最多 64 条";
  if (effective.some((word) => [...word].length > 32)) return "每条热词最多 32 个字符";
  if (new TextEncoder().encode(effective.join("")).length > 4096) return "热词总长度超过 4096 字节";
  const customError = customPersonasError(form.custom_personas);
  if (customError) return customError;
  const style = String(form.persona_style || "workshop").trim();
  if (!personaOptions(form.custom_personas).some((opt) => opt.id === style)) return "请选择有效的说话角色";
  const cardStyle = String(form.screen_card_style || "moss_hud").trim();
  if (!["moss_hud", "goggle_bot"].includes(cardStyle)) return "请选择有效的屏幕卡片画风";
  const prompt = String(form.persona_prompt || "").replace(/\r\n?/g, "\n");
  if (/[\x00-\x09\x0b-\x1f\x7f]/.test(prompt)) return "补充说明不能包含控制字符";
  if ([...prompt.trim()].length > 300) return "补充说明最多 300 个字符";
  if (containsDangerousGcodeOrGrblJs(prompt)) return "补充说明不能包含 G-code 或控制器 $n= 指令";
  const corrLines = String(form.custom_asr_corrections || "").split(/\r?\n/);
  const seenWrong = new Map();
  for (const line of corrLines) {
    const parsed = parseAsrCorrectionLine(line);
    if (parsed.empty) continue;
    if (parsed.error) return parsed.error;
    seenWrong.set(parsed.wrong, parsed.formatted);
  }
  if (seenWrong.size > 64) return "自定义同音纠错最多 64 条";
  if (new TextEncoder().encode([...seenWrong.values()].join("")).length > 4096) return "同音纠错总长度超过 4096 字节";
  return "";
}
