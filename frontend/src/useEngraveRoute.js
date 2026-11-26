// 雕刻路线全局开关的读写与页面同步（2026-11-17，技术书 r45）：值只存在后端，这里负责
// 读取（挂载、窗口重新获得焦点 / 切回可见时）、保存（失败回到原值并提示）和本窗口内广播，
// 工作台与创作画板各自用这个 hook，看到的始终是同一个值。纯逻辑见 engraveRoute.js。

import { useCallback, useEffect, useRef, useState } from "react";
import { getJson, postJson } from "./api/client.js";
import { showAlert } from "./appDialog.js";
import { isReadOnlyReferenceClient } from "./authSession.js";
import {
  DEFAULT_ENGRAVE_ROUTE,
  ENGRAVE_ROUTE_EVENT,
  engraveRouteFromResponse,
  engraveRouteSaveOutcome,
  normalizeEngraveRoute,
} from "./engraveRoute.js";

// 最近一次读到 / 保存的值：换页面时新挂载的开关先显示它，再等后端回读
let lastKnownRoute = null;
// 保存进行中时忽略回读结果，避免旧值把刚点的新值冲回去
let pendingSaves = 0;

function broadcast(route) {
  lastKnownRoute = route;
  try {
    window.dispatchEvent(new CustomEvent(ENGRAVE_ROUTE_EVENT, { detail: { route } }));
  } catch (_error) {
    // 非浏览器环境：只记住值
  }
}

/** 从后端读一次；读到新值时广播给本窗口内的其他开关。失败返回 null。 */
export async function fetchEngraveRoute() {
  const res = await getJson("/api/engrave-route");
  const route = engraveRouteFromResponse(res);
  if (route && pendingSaves === 0 && route !== lastKnownRoute) broadcast(route);
  return pendingSaves === 0 ? route : null;
}

/**
 * `{ route, setRoute, saving, readOnly }`：`setRoute` 先切换界面，再保存到后端；失败时回到原值并弹窗说明。
 * 连接电脑（局域网）时只读。
 */
export function useEngraveRoute() {
  const [route, setRouteState] = useState(() => lastKnownRoute || DEFAULT_ENGRAVE_ROUTE);
  const [saving, setSaving] = useState(false);
  const routeRef = useRef(route);
  routeRef.current = route;
  const readOnly = isReadOnlyReferenceClient();

  useEffect(() => {
    let alive = true;
    const onRoute = (event) => {
      if (alive) setRouteState(normalizeEngraveRoute(event?.detail?.route));
    };
    const refresh = () => {
      fetchEngraveRoute().then((next) => {
        if (alive && next) setRouteState(next);
      }).catch(() => {});
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") refresh();
    };
    window.addEventListener(ENGRAVE_ROUTE_EVENT, onRoute);
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", onVisibility);
    refresh();
    return () => {
      alive = false;
      window.removeEventListener(ENGRAVE_ROUTE_EVENT, onRoute);
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);

  const setRoute = useCallback(async (next) => {
    const target = normalizeEngraveRoute(next);
    const previous = routeRef.current;
    if (readOnly || target === previous) return;
    pendingSaves += 1;
    setSaving(true);
    broadcast(target);
    let outcome;
    try {
      outcome = engraveRouteSaveOutcome(await postJson("/api/engrave-route", { route: target }), previous);
    } catch (error) {
      outcome = engraveRouteSaveOutcome({ ok: false, error: error?.message || String(error) }, previous);
    } finally {
      pendingSaves -= 1;
      setSaving(false);
    }
    broadcast(outcome.route);
    if (outcome.error) void showAlert(outcome.error, { title: "雕刻路线没有切换", tone: "warning" });
  }, [readOnly]);

  return { route, setRoute, saving, readOnly };
}
