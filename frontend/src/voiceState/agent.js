// 本机 Agent（Claude Code / Codex）卡片显示。

// ---------- Agent（Claude Code / Codex，2026-10-15 起可用） ----------

const AGENT_ERROR_TEXT = {
  agent_disabled: "未开启",
  agent_strict_mode_required: "需要先开启严格模式",
  agent_path_denied: "路径不可用（工作目录或程序路径不符合要求）",
  agent_capability_missing: "无法识别这个 CLI，或缺少需要的安全参数",
  agent_run_failed: "运行失败",
  agent_timeout: "超时（5 分钟）已停止",
  agent_stopped: "已手动停止",
  agent_permission_expired: "设备授权或配置变化，已停止",
  agent_sandbox_unavailable: "当前系统不支持",
  agent_thread_spawn_failed: "Agent 任务线程启动失败，已记为失败",
};

export function agentErrorText(code) {
  return AGENT_ERROR_TEXT[code] || code || "";
}

const RUN_STATUS = { running: "运行中", completed: "已完成", failed: "失败", cancelled: "已停止" };

/** Agent 卡的显示：总状态、每个 CLI 能否用、每条运行记录。 */
export function agentView(info) {
  const providers = (Array.isArray(info?.providers) ? info.providers : []).map((item) => {
    const sourceTag = item.source === "auto_fallback"
      ? " · 已自动适配当前设备"
      : item.source === "auto"
        ? " · 自动读取"
        : "";
    return {
      name: item.provider === "codex" ? "Codex" : "Claude Code",
      ready: Boolean(item.ready),
      source: item.source || "configured",
      executable: item.executable || "",
      text: item.ready
        ? `可用${item.version ? `（${item.version}）` : ""}${sourceTag}`
        : agentErrorText(item.error_code),
    };
  });
  const summary = !info?.configured ? "未开启" : !info?.strict_mode ? "已开启 · 需要严格模式" : providers.some((p) => p.ready) ? "可用" : "已开启 · 没有可用的 CLI";
  const runs = (Array.isArray(info?.runs) ? info.runs : []).slice(0, 50).map((run) => ({
    ...run,
    statusText: RUN_STATUS[run.status] || run.status,
    detail: run.output ? String(run.output) : agentErrorText(run.error_code) || "无结果",
  }));
  return { summary, providers, runs, strictMissing: Boolean(info?.configured) && !info?.strict_mode };
}
