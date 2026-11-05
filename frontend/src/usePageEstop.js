import { useCallback, useEffect, useRef } from "react";
import { postJson } from "./api/client.js";
import { onEmergencyStop } from "./estopBus.js";
import { createPageEstopResponder, ESTOP_SEND_TIMEOUT_MS, stopPageTarget } from "./globalEstop.js";

/** 急停写出请求：只走 `/api/safe-action`，带短时限，挂起时也能尽快轮到下一条通道（2026-11-05）。 */
export function sendEstopRequest(path, body) {
  return postJson(path, body, { timeoutMs: ESTOP_SEND_TIMEOUT_MS });
}

/**
 * 页面急停（工作台 / 画板 / 数字孪生共用，2026-11-05）：
 * - `getContext()` 返回 `{ jobId, target }`（`target` 为 `estopTargetArgs` 形状或 null）；
 * - `report(outcome)` 展示 `stopPageTarget` 的结果；
 * - 返回的函数供页面急停按钮直接调用；同时注册顶栏急停事件，按同一机制补发 0x18。
 * 不加确认框：急停路径不得多一步。
 */
export function usePageEmergencyStop(getContext, report) {
  const latest = useRef({ getContext, report });
  latest.current = { getContext, report };

  const run = useCallback(async (context) => {
    const outcome = await stopPageTarget(sendEstopRequest, context || {});
    await latest.current.report?.(outcome);
    return outcome;
  }, []);

  useEffect(
    () => onEmergencyStop(createPageEstopResponder(() => latest.current.getContext?.(), run)),
    [run],
  );

  // 按钮 onClick 会传入事件对象，这里忽略参数、每次读取最新的页面目标
  return useCallback(() => run(latest.current.getContext?.() || {}), [run]);
}
