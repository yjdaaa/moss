/**
 * 工作台“扫描局域网”结果处理（2026-11-09 用户要求）：后端只返回按 GRBL / FluidNC 回应了只读查询（`?` + `$I`）的设备。
 * 只找到 1 台时自动填入；找到多台时不替用户选，列出来让用户挑；一台都没有时说明原因。
 */

const STATE_LABELS = {
  Idle: "空闲",
  Run: "加工中",
  Hold: "暂停",
  Jog: "点动中",
  Alarm: "报警锁定",
  Door: "门开",
  Check: "检查模式",
  Home: "回零中",
  Sleep: "休眠",
  Starting: "启动中",
};

/** 一台控制器的简短说明：`192.0.2.20:23 · Yisu V1.0 · fluidnc.local · FluidNC v4.1.1 · 空闲`（机器名来自 FluidNC `$I`） */
export function controllerLabel(controller) {
  const parts = [`${controller.ip}:${controller.telnet_port}`];
  if (controller.machine) parts.push(controller.machine);
  if (controller.hostname) parts.push(`${controller.hostname}.local`);
  if (controller.firmware) parts.push(controller.firmware);
  if (controller.state) parts.push(STATE_LABELS[controller.state] || controller.state);
  return parts.join(" · ");
}

/**
 * 把扫描结果变成界面动作：`fill`（要填入的控制器或 null）、`choices`（多台时给用户选的列表）与日志 `{text, level}`。
 */
export function discoveryOutcome(result) {
  const controllers = Array.isArray(result?.controllers) ? result.controllers : [];
  const busy = Array.isArray(result?.busy) ? result.busy : [];
  const busyNote = busy.length ? `；另有 ${busy.length} 台正在加工，未另开连接打扰` : "";
  if (controllers.length === 1) {
    const only = controllers[0];
    return {
      fill: only,
      choices: [],
      log: {
        text: `找到 1 台雕刻机控制器：${controllerLabel(only)}，已填入；可点“握手探测”确认激光模式${busyNote}`,
        level: "ok",
      },
    };
  }
  if (controllers.length > 1) {
    return {
      fill: null,
      choices: controllers,
      log: {
        text: `找到 ${controllers.length} 台雕刻机控制器，请在下方选择要连接的一台：${controllers.map(controllerLabel).join("；")}${busyNote}`,
        level: "ok",
      },
    };
  }
  return {
    fill: null,
    choices: [],
    log: { text: result?.hint || "没有设备按 GRBL / FluidNC 回应", level: "warn" },
  };
}
