/**
 * 高频轮询守卫（2026-11-05）：setInterval 每个节拍调用 `tick()`。
 * - 上一次请求还没返回时跳过本节拍，不叠加并发请求；
 * - `task(isCurrent)` 收到 `isCurrent()`，在 `invalidate()`（卸载、切换间隔）之后返回的旧响应应丢弃。
 * `tick()` 返回 Promise<boolean>：true 表示本节拍真正发出了请求。
 */
export function createPollRunner(task) {
  let pending = false;
  let generation = 0;
  const tick = async () => {
    if (pending) return false;
    pending = true;
    const mine = generation;
    try {
      await task(() => mine === generation);
    } catch (_error) {
      // 由 task 自己处理错误；守卫只负责释放占用
    } finally {
      pending = false;
    }
    return true;
  };
  tick.invalidate = () => {
    generation += 1;
  };
  tick.isPending = () => pending;
  return tick;
}
