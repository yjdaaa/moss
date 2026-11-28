/**
 * LB5 / LD 语音中心「AI 联调与测试 › 激光雕刻机测试」状态格式化、空跑确认文案与精简提示词纯函数（不依赖 React，便于单测）。
 * 严禁在输出中包含任何令牌、密钥、设备地址（IP / 串口号）或本机绝对路径。
 */

export const LASER_BENCH_PROMPT_MAX_LENGTH = 1500;
export const DRY_RUN_DEFAULT_MAX_FEED = 2000;
export const DRY_RUN_MIN_FEED = 300;
export const DRY_RUN_MAX_FEED = 5000;

export const DRY_RUN_CONFIRM_MESSAGE =
  "开启真机空跑模式后：不出光，所有移动限速，急停照常可用，60 分钟后自动关闭。可随时手动关闭。确定开启吗？";

/** 格式化空跑剩余时长（秒 → 中文短语）。 */
export function formatDryRunRemaining(remainingSeconds) {
  const secs = Math.max(0, Math.floor(Number(remainingSeconds) || 0));
  if (secs <= 0) return "已到期";
  if (secs < 60) return "不足 1 分钟";
  return `${Math.ceil(secs / 60)} 分钟`;
}

/** 生成卡片顶部的单行状态，例如“已连接 · 空跑关闭”或“已连接 · 空跑开启（限速 2000 mm/min · 剩余 58 分钟）”。 */
export function formatLaserBenchStatusLine({ connected = false, dryRun = null } = {}) {
  const connText = connected ? "已连接" : "未连接";
  if (dryRun?.enabled) {
    const feed = Number(dryRun.max_feed_mm_min) || DRY_RUN_DEFAULT_MAX_FEED;
    return `${connText} · 空跑开启（限速 ${feed} mm/min · 剩余 ${formatDryRunRemaining(dryRun.remaining_seconds)}）`;
  }
  return `${connText} · 空跑关闭`;
}

/** 顶栏醒目空跑标记文案；关闭时返回空串。 */
export function formatDryRunBannerText(dryRun) {
  if (!dryRun?.enabled) return "";
  const feed = Number(dryRun.max_feed_mm_min) || DRY_RUN_DEFAULT_MAX_FEED;
  return `空跑模式（不出光 · 限速 ${feed} mm/min · 剩余 ${formatDryRunRemaining(dryRun.remaining_seconds)}）`;
}

function stripSecretsPathsAndAddrs(text) {
  return String(text ?? "")
    .replace(/\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{6,}/gi, "$1 [REDACTED]")
    .replace(/\b(?:sk|rk|pk)-[A-Za-z0-9_-]{16,}\b/g, "[REDACTED]")
    .replace(/(?:\/Users|\/home|\/var\/folders|\/private\/var|\/tmp)\/[^\s"'<>`,;)\]]+/g, "<path>")
    .replace(/\b[A-Za-z]:\\(?:Users|Documents and Settings|ProgramData|Temp|Windows)[^\s"'<>`,;)\]]*/gi, "<path>")
    .replace(/\/dev\/(?:cu|tty)\.[A-Za-z0-9._-]+/g, "<serial-port>")
    .replace(/\/dev\/tty(?:USB|ACM|S)\d+/g, "<serial-port>")
    .replace(/\bCOM\d{1,3}\b/gi, "<serial-port>")
    .replace(/\b(?:\d{1,3}\.){3}\d{1,3}\b/g, (ip) => (ip === "127.0.0.1" ? ip : "<device-ip>"));
}

/**
 * 生成可直接粘贴给 AI 代理的激光自主测试精简提示词。
 * 不重复项目规范，不包含令牌、绝对路径或设备地址。
 */
export function buildLaserBenchPrompt(options = {}) {
  const profile = options?.profile === "grbl" ? "grbl" : "fluidnc";
  const profileArg = profile === "grbl" ? " --profile grbl" : "";

  const prompt = [
    "# 激光雕刻机 AI 自主测试",
    "",
    "遵守 `AGENTS.md` 与 `docs/developer/safety.md`。",
    "",
    "## 命令与用途",
    `- L0 模拟机全自动（隔离运行目录 + 回环模拟器，零硬件风险）：\`node scripts/laser-bench/laser-sim-bench.mjs${profileArg}\``,
    "- L1 真机只读核对（白名单仅握手与状态查询，前提：用户已在 App 连接机器）：`node scripts/laser-bench/laser-readonly-check.mjs`",
    "- L2 真机空跑（后端强制不出光、限速；前提：用户已在 App 连接机器并在「语音中心 › 记录与诊断 › AI 联调与测试 › 激光雕刻机测试」打开空跑开关；出光永远由用户手动）：`node scripts/laser-bench/laser-dryrun-bench.mjs`",
    "- 前提未满足时暂停并告知用户，不自行改连接或绕过开关。",
    "- 归档位置：`<runtime>/laser-bench/{sim,readonly,dryrun}-<run-id>/`，字段见 `scripts/laser-bench/README.md`。",
    "",
    "## 流程",
    "如果你支持子代理，按以下角色分工；不支持就由你自己按同样的阶段顺序执行。",
    "1. 诊断（只测不改）：按前提依次运行 L0 / L1 / L2，输出按严重程度排序的问题清单（安全类最高），附证据与怀疑的代码位置。",
    "2. 对症修复：一次只处理一个问题，交给修复子代理，只给该问题证据与相关文件，做最小改动并补测试。",
    "3. 独立复测：由未参与修复的一方重跑对应测试与单测，确认问题消除且无回归再保留，否则撤回。",
    "4. 提交前审查：由未参与修复的审查子代理只看 diff，确认无误后再本地提交。",
    "",
    "最多 3 轮。",
    "",
    "## 汇报",
    "- 开头先给出「问题总数」与「不变量违反（安全类）数量」。",
    "- 随后列出通过率、问题清单与证据、每轮修复与复测结论，以及仍需用户决定的项。",
  ].join("\n");

  return stripSecretsPathsAndAddrs(prompt);
}
