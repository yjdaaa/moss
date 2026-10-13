/**
 * 顶栏常驻急停（技术书 §9.5.5 A）：与各页面原有急停 / 取消按钮调用同一安全动作。
 *
 * 顶栏不持有任务状态，所以通过事件把请求交给当前页面：
 * - 页面用 onEmergencyStop 注册处理函数；处理函数返回 true 表示它确实对一个任务发起了停止；
 * - requestEmergencyStop 返回是否有页面处理。没有页面处理时由调用方提示用户使用硬件急停，
 *   绝不假装已经停机。
 */
export const ESTOP_EVENT = "moss:estop-request";

export function requestEmergencyStop(target = globalThis.window) {
  const detail = { handled: false };
  if (target?.dispatchEvent) {
    target.dispatchEvent(new CustomEvent(ESTOP_EVENT, { detail }));
  }
  return detail.handled;
}

export function onEmergencyStop(handler, target = globalThis.window) {
  if (!target?.addEventListener) return () => {};
  const listener = (event) => {
    if (event.detail?.handled) return;
    if (handler() === true) event.detail.handled = true;
  };
  target.addEventListener(ESTOP_EVENT, listener);
  return () => target.removeEventListener(ESTOP_EVENT, listener);
}
