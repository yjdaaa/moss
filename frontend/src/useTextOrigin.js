// 文字对刀起点全局设置的读写与页面同步（2026-11-17，技术书 r48；做法同 useEngraveRoute.js）：值只存在后端，
// 挂载、窗口重新获得焦点 / 切回可见时重读；改了先切换界面再保存，失败回到原值并提示。
// 局域网（手机连电脑）只读：改动只对本次生效，不保存、不弹窗。纯逻辑见 textOrigin.js。

import { useCallback, useEffect, useRef, useState } from "react";
import { getJson, postJson } from "./api/client.js";
import { showAlert } from "./appDialog.js";
import { isReadOnlyReferenceClient } from "./authSession.js";
import {
  DEFAULT_TEXT_ORIGIN,
  TEXT_ORIGIN_EVENT,
  normalizeTextOrigin,
  textOriginFromResponse,
  textOriginSaveOutcome,
} from "./textOrigin.js";

let lastKnownOrigin = null;
let pendingSaves = 0;

function broadcast(origin) {
  lastKnownOrigin = origin;
  try {
    window.dispatchEvent(new CustomEvent(TEXT_ORIGIN_EVENT, { detail: { origin } }));
  } catch (_error) {
    // 非浏览器环境：只记住值
  }
}

/** 从后端读一次；读到新值时广播。失败返回 null。 */
export async function fetchTextOrigin() {
  const res = await getJson("/api/text-origin");
  const origin = textOriginFromResponse(res);
  if (origin && pendingSaves === 0 && origin !== lastKnownOrigin) broadcast(origin);
  return pendingSaves === 0 ? origin : null;
}

/** `{ origin, setOrigin, readOnly }`。 */
export function useTextOrigin() {
  const readOnly = isReadOnlyReferenceClient();
  const [origin, setOriginState] = useState(() => lastKnownOrigin || DEFAULT_TEXT_ORIGIN);
  const originRef = useRef(origin);
  useEffect(() => {
    originRef.current = origin;
  });

  useEffect(() => {
    let alive = true;
    const onOrigin = (event) => {
      if (alive) setOriginState(normalizeTextOrigin(event?.detail?.origin));
    };
    const refresh = () => {
      fetchTextOrigin().then((next) => {
        if (alive && next) setOriginState(next);
      }).catch(() => {});
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") refresh();
    };
    window.addEventListener(TEXT_ORIGIN_EVENT, onOrigin);
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", onVisibility);
    refresh();
    return () => {
      alive = false;
      window.removeEventListener(TEXT_ORIGIN_EVENT, onOrigin);
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);

  const setOrigin = useCallback(async (next) => {
    const target = normalizeTextOrigin(next);
    const previous = originRef.current;
    if (target === previous) return;
    if (readOnly) {
      // 手机连电脑：只改这一次，请求里照样带上
      setOriginState(target);
      return;
    }
    pendingSaves += 1;
    broadcast(target);
    let outcome;
    try {
      outcome = textOriginSaveOutcome(await postJson("/api/text-origin", { origin: target }), previous);
    } catch (error) {
      outcome = textOriginSaveOutcome({ ok: false, error: error?.message || String(error) }, previous);
    } finally {
      pendingSaves -= 1;
    }
    broadcast(outcome.origin);
    if (outcome.error) void showAlert(outcome.error, { title: "对刀起点没有切换", tone: "warning" });
  }, [readOnly]);

  return { origin, setOrigin, readOnly };
}
