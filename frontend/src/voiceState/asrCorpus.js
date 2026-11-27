/**
 * VB1/VB2 识别（ASR）真实语料采集与人工标注面板的纯状态逻辑。
 */

export const ASR_CORPUS_TAGS = ["命令词", "数字", "材料名", "有噪声"];

export const ASR_CORPUS_ENABLE_CONFIRM_TEXT =
  "开启后，真小智说话解码后的 16 kHz 单声道 WAV 录音与识别记录将仅保存在本机运行目录 asr-corpus/ 中（默认最多保留 500 条或 7 天，先到先清）。\n\n" +
  "• 仅用于本机离线识别测评与标注；\n" +
  "• 不会写入诊断包，也不会写入过程记录附件；\n" +
  "• 绝不用于声纹识别或身份授权；模拟联调轮次不会保存。\n\n" +
  "确认开启真实识别录音采集？";

export const ASR_CORPUS_ENABLE_CONFIRM_OPTIONS = {
  title: "开启识别录音语料采集",
  tone: "warning",
  confirmText: "确认开启",
  cancelText: "取消",
};

export const ASR_CORPUS_DISABLE_CHOICE_TEXT =
  "关闭后将停止保存新的识别录音。\n\n是否同时删除本机已采集的全部识别语料与标注？";

export const ASR_CORPUS_DISABLE_CHOICE_OPTIONS = {
  title: "关闭识别录音语料采集",
  tone: "warning",
  confirmText: "关闭并删除已采集语料",
  cancelText: "仅关闭，保留已有语料",
  danger: true,
};

export const ASR_CORPUS_DELETE_ITEM_CONFIRM_TEXT =
  "确定删除这条识别录音及其标注文件？删除后不可恢复。";

export const ASR_CORPUS_DELETE_ITEM_CONFIRM_OPTIONS = {
  title: "删除识别语料",
  tone: "warning",
  confirmText: "确认删除",
  cancelText: "取消",
  danger: true,
};

export const ASR_CORPUS_CLEAR_ALL_CONFIRM_TEXT =
  "确定清空本机全部已采集的识别录音与标注？清空后不可恢复。";

export const ASR_CORPUS_CLEAR_ALL_CONFIRM_OPTIONS = {
  title: "清空全部识别语料",
  tone: "warning",
  confirmText: "确认清空",
  cancelText: "取消",
  danger: true,
};

/**
 * 归一化语料面板视图状态；开关关闭时 panelUsable 恒为 false。
 */
export function asrCorpusView(data) {
  const enabled = Boolean(data?.enabled);
  const maxItems = Number(data?.max_items) || 500;
  const maxDays = Number(data?.max_days) || 7;
  const totalCount = Number(data?.total_count) || 0;
  const labeledCount = Number(data?.labeled_count) || 0;
  const items = enabled && Array.isArray(data?.items) ? data.items : [];
  const corpusDirRel = String(data?.corpus_dir_rel || "asr-corpus");
  const summary = enabled
    ? `采集已开 · ${totalCount} 条（已标注 ${labeledCount} 条，上限 ${maxItems} 条 / ${maxDays} 天）`
    : totalCount > 0
      ? `采集已关（本机留存 ${totalCount} 条）`
      : "默认关闭（隐私优先）";
  return {
    enabled,
    panelUsable: enabled,
    maxItems,
    maxDays,
    totalCount,
    labeledCount,
    items,
    corpusDirRel,
    summary,
  };
}

export function toggleAsrCorpusTag(tags, tag) {
  if (!ASR_CORPUS_TAGS.includes(tag)) return Array.isArray(tags) ? [...tags] : [];
  const list = Array.isArray(tags) ? tags.filter((t) => ASR_CORPUS_TAGS.includes(t)) : [];
  if (list.includes(tag)) {
    return list.filter((t) => t !== tag);
  }
  return [...list, tag];
}

export function buildAsrCorpusLabelPayload(item, referenceText, tags = [], verifiedCorrect = false) {
  if (!item?.date || !item?.turn_id) {
    return { error: "缺少语料日期或编号" };
  }
  const text = String(referenceText ?? "").trim();
  if (!text) {
    return { error: "正确文字不能为空" };
  }
  if ([...text].length > 500) {
    return { error: "正确文字最多 500 个字符" };
  }
  const cleanTags = [];
  for (const tag of Array.isArray(tags) ? tags : []) {
    if (ASR_CORPUS_TAGS.includes(tag) && !cleanTags.includes(tag)) {
      cleanTags.push(tag);
    }
  }
  return {
    payload: {
      action: "save_label",
      date: item.date,
      turn_id: item.turn_id,
      reference_text: text,
      verified_correct: Boolean(verifiedCorrect),
      tags: cleanTags,
    },
  };
}
