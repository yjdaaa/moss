import { useCallback, useEffect, useRef, useState } from "react";
import { showAlert } from "../../appDialog.js";
import { VOICE_BUILTIN_STATUS_EVENT, voiceApi } from "../../voiceApi.js";
import { createPollRunner } from "../../pollGuard.js";
import {
  VOICE_POLL_MS,
  voiceErrorText,
} from "../../voiceState.js";

/** 内置服务状态：每 4 秒刷新；`busy` 在本服务的各部分之间共享，避免并发操作。 */
export function useBuiltinStatus() {
  const [status, rawSetStatus] = useState(null);
  const [busy, setBusy] = useState("");
  const mounted = useRef(true);
  const versionRef = useRef(0);
  const staticGifsRef = useRef({ goggle_bot_gifs: null, screen_style_gifs: null });

  const mergeStaticGifs = useCallback((next) => {
    if (!next || typeof next !== "object") return next;
    if (next.goggle_bot_gifs) staticGifsRef.current.goggle_bot_gifs = next.goggle_bot_gifs;
    if (next.screen_style_gifs) staticGifsRef.current.screen_style_gifs = next.screen_style_gifs;
    const needBot = !next.goggle_bot_gifs && Boolean(staticGifsRef.current.goggle_bot_gifs);
    const needStyle = !next.screen_style_gifs && Boolean(staticGifsRef.current.screen_style_gifs);
    if (needBot || needStyle) {
      return {
        ...next,
        ...(needBot ? { goggle_bot_gifs: staticGifsRef.current.goggle_bot_gifs } : {}),
        ...(needStyle ? { screen_style_gifs: staticGifsRef.current.screen_style_gifs } : {}),
      };
    }
    return next;
  }, []);

  const setStatus = useCallback((next) => {
    versionRef.current += 1;
    rawSetStatus((prev) => {
      const resolved = typeof next === "function" ? next(prev) : next;
      return mergeStaticGifs(resolved);
    });
  }, [mergeStaticGifs]);

  // 2026-11-05：对话进行中按 140ms 轮询；上一次请求未返回时跳过本节拍，卸载后与动作改写状态后丢弃旧响应
  const pollRef = useRef(null);
  if (!pollRef.current) {
    pollRef.current = createPollRunner(async (isCurrent) => {
      const ver = versionRef.current;
      const fresh = () => mounted.current && isCurrent() && versionRef.current === ver;
      try {
        const useLite = Boolean(
          staticGifsRef.current.goggle_bot_gifs && staticGifsRef.current.screen_style_gifs,
        );
        const next = await voiceApi.builtinStatus(useLite ? { lite: true } : undefined);
        if (!useLite && next && !next.error) {
          if (next.goggle_bot_gifs) staticGifsRef.current.goggle_bot_gifs = next.goggle_bot_gifs;
          if (next.screen_style_gifs) staticGifsRef.current.screen_style_gifs = next.screen_style_gifs;
        }
        if (fresh()) rawSetStatus(mergeStaticGifs(next));
      } catch (error) {
        // 读过一次后保留上次状态，只标出刷新失败；不卸载各分组，避免丢失未保存的填写
        if (fresh()) {
          rawSetStatus((prev) => (prev && !prev.error ? { ...prev, poll_error: voiceErrorText(error) } : { error: voiceErrorText(error) }));
        }
      }
    });
  }
  const refresh = useCallback(() => pollRef.current(), []);

  const activeTurn = Boolean(
    status?.running
    && Array.isArray(status?.sessions)
    && status.sessions.some((item) => item?.state === "listening" || item?.state === "thinking" || item?.state === "speaking"),
  );

  useEffect(() => {
    mounted.current = true;
    void refresh();
    const onExternalStatus = (event) => {
      const cmd = event?.detail?.command;
      const next = event?.detail?.status;
      if (
        mounted.current &&
        next &&
        (cmd === "voice_device_set_status" || cmd === "voice_device_rename" || cmd === "voice_device_remove" || cmd === "voice_local_save")
      ) {
        setStatus(next);
      }
    };
    if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
      window.addEventListener(VOICE_BUILTIN_STATUS_EVENT, onExternalStatus);
    }
    return () => {
      mounted.current = false;
      pollRef.current?.invalidate();
      if (typeof window !== "undefined" && typeof window.removeEventListener === "function") {
        window.removeEventListener(VOICE_BUILTIN_STATUS_EVENT, onExternalStatus);
      }
    };
  }, [refresh, setStatus]);

  useEffect(() => {
    const intervalMs = activeTurn ? 140 : VOICE_POLL_MS;
    const timer = setInterval(() => {
      if (!document.hidden) void refresh();
    }, intervalMs);
    const onVisibility = () => {
      if (!document.hidden) void refresh();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [activeTurn, refresh]);
  const act = useCallback(async (label, action) => {
    setBusy(label);
    try {
      const next = await action();
      if (mounted.current && next) setStatus(next);
      return next;
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: `${label}失败`, tone: "error" });
      return null;
    } finally {
      if (mounted.current) setBusy("");
    }
  }, [setStatus]);

  return { status, setStatus, busy, setBusy, act, refresh };
}
