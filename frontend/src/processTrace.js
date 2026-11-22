/** 完整过程记录（n105 / r55，默认关）设置页的纯逻辑与文案。 */

/** 字节数 → “12.3 MB”这类短文本。 */
export function formatTraceBytes(bytes) {
  const value = Number(bytes) || 0;
  if (value < 1024) return `${value} B`;
  const units = ["KB", "MB", "GB"];
  let size = value / 1024;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size >= 100 ? size.toFixed(0) : size.toFixed(1)} ${units[unit]}`;
}

/** 状态行：开关、占用、天数与保留规则。 */
export function processTraceSummary(status) {
  if (!status) return "读取中…";
  const keep = `只保留最近 ${status.retention_days} 天，最多 ${formatTraceBytes(status.max_bytes)}`;
  if (!status.bytes) return `目前没有记录。${keep}。`;
  return `已记录 ${status.days} 天，共 ${formatTraceBytes(status.bytes)}。${keep}。`;
}

export const PROCESS_TRACE_CONFIRM =
  "打开后，MOSS 会把下面这些内容保存在这台电脑上，方便事后核对：\n" +
  "· 你对小智说话的录音、模型识别出的文字（原文和纠正后）与设备拍照看图的照片\n" +
  "· 发给大模型的完整内容（系统提示词、上下文、工具清单）和模型的原始回复\n" +
  "· 工具调用的参数与完整结果、MOSS 合成的语音、每一步耗时与诊断事件\n" +
  "· 激光生成的参数、发起方、G-code 与预览图，以及连接、确认发送、急停、审批、加工进度与结果等每一步操作\n\n" +
  "所有步骤按先后写成一条时间线，每一步都写明在干什么。" +
  "不保存任何密钥或令牌，也不会上传到任何地方。记录只保留最近 7 天，到期自动删除，也可以随时清空。";
