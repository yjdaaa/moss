import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Camera,
  RefreshCw,
  FlipHorizontal2,
  RotateCw,
  Trash2,
  Radio,
  Play,
  Pause,
  AlertTriangle,
  Sparkles,
  ArrowUpRight,
} from "lucide-react";
import { createPollRunner } from "../pollGuard.js";
import { voiceApi } from "../voiceApi.js";
import { formatBytes, voiceErrorText } from "../voiceState.js";
import {
  CAMERA_AUTO_GAP_MS,
  CAMERA_AUTO_INTERVAL_MS,
  CAMERA_FPS_WINDOW,
  CAMERA_FRAME_POLL_MS,
  CAMERA_STATUS_POLL_MS,
  cameraDevicesView,
  cameraFpsView,
  cameraFrameSourceView,
  cameraTransformValue,
  shouldAutoCaptureNext,
} from "../cameraState.js";

const VIEW_PREFS_KEY = "moss_camera_view_prefs";
const TONE_CLASS = {
  ok: "ob-pill--ok",
  info: "ob-pill--info",
  warn: "ob-pill--warn",
  bad: "ob-pill--bad",
  off: "ob-pill--off",
};

function readViewPrefs() {
  try {
    const raw = globalThis.localStorage?.getItem(VIEW_PREFS_KEY);
    if (!raw) return { mirror: false, flip180: false };
    const parsed = JSON.parse(raw);
    return {
      mirror: Boolean(parsed?.mirror),
      flip180: Boolean(parsed?.flip180),
    };
  } catch {
    return { mirror: false, flip180: false };
  }
}

function writeViewPrefs(next) {
  try {
    globalThis.localStorage?.setItem(VIEW_PREFS_KEY, JSON.stringify(next));
  } catch {
    // 存储受限时仅在当前会话生效
  }
}

function formatCaptureTime(seconds) {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleTimeString("zh-CN", {
    hour12: false,
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/**
 * 画面淡入切换（2026-11-09）：新图先在后台解码好再叠在旧图上淡入，换帧不闪不跳。
 * 最上面一层带 `#camera-frame-img`；淡入结束后只留这一层。
 */
function CameraFrame({ src, alt, transform }) {
  const [layers, setLayers] = useState(() => (src ? [{ src, key: 0 }] : []));
  const keyRef = useRef(0);
  useEffect(() => {
    if (!src) {
      setLayers([]);
      return undefined;
    }
    let cancelled = false;
    const show = () => {
      if (cancelled) return;
      setLayers((prev) => {
        if (prev.length && prev[prev.length - 1].src === src) return prev;
        keyRef.current += 1;
        return [...prev.slice(-1), { src, key: keyRef.current }];
      });
    };
    const img = new Image();
    img.src = src;
    if (typeof img.decode === "function") img.decode().then(show, show);
    else show();
    return () => {
      cancelled = true;
    };
  }, [src]);
  if (!layers.length) return null;
  const front = layers[layers.length - 1];
  return (
    <div className="camera-frame-stack">
      {layers.map((layer) => {
        const isFront = layer.key === front.key;
        return (
          <img
            key={layer.key}
            id={isFront ? "camera-frame-img" : undefined}
            src={layer.src}
            alt={isFront ? alt : ""}
            aria-hidden={isFront ? undefined : "true"}
            className={`camera-frame-img${isFront && layers.length > 1 ? " is-entering" : ""}`}
            style={transform ? { transform } : undefined}
            onAnimationEnd={isFront ? () => setLayers((prev) => prev.slice(-1)) : undefined}
          />
        );
      })}
    </div>
  );
}

export function CameraPage({ onNavigate }) {
  const [builtinStatus, setBuiltinStatus] = useState(null);
  const [statusError, setStatusError] = useState("");
  const [selectedDeviceId, setSelectedDeviceId] = useState("");
  const [frame, setFrame] = useState(null);
  const [frameMeta, setFrameMeta] = useState(null);
  const [capturing, setCapturing] = useState(false);
  const [autoRefresh, setAutoRefresh] = useState(false);
  const [actionError, setActionError] = useState("");
  const [startingServer, setStartingServer] = useState(false);
  const [viewPrefs, setViewPrefs] = useState(() => readViewPrefs());
  const [fpsView, setFpsView] = useState(null);
  const arrivalsRef = useRef([]);

  const mountedRef = useRef(true);
  const selectedDeviceRef = useRef("");
  const frameSeqRef = useRef(null);
  const lastCaptureTriggeredAtRef = useRef(0);
  const captureReqIdRef = useRef(0);
  const activeFramePollRef = useRef(null);
  const dismissedErrorSeqRef = useRef(null);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const view = useMemo(() => cameraDevicesView(builtinStatus), [builtinStatus]);

  useEffect(() => {
    if (!view.devices.length) {
      if (selectedDeviceId !== "") setSelectedDeviceId("");
      return;
    }
    const stillExists = view.devices.some((d) => d.deviceId === selectedDeviceId);
    if (!selectedDeviceId || !stillExists) {
      setSelectedDeviceId(view.defaultDeviceId);
    }
  }, [view.devices, view.defaultDeviceId, selectedDeviceId]);

  const activeDevice = useMemo(
    () => view.devices.find((d) => d.deviceId === selectedDeviceId) || null,
    [view.devices, selectedDeviceId],
  );

  // 切换目标设备时重置画面与自动刷新，绝不静默触发另一台设备的摄像头
  useEffect(() => {
    selectedDeviceRef.current = selectedDeviceId;
    captureReqIdRef.current += 1;
    activeFramePollRef.current?.invalidate?.();
    frameSeqRef.current = null;
    dismissedErrorSeqRef.current = null;
    setCapturing(false);
    setFrame(null);
    setFrameMeta(null);
    setActionError("");
    setAutoRefresh(false);
    arrivalsRef.current = [];
    setFpsView(null);
  }, [selectedDeviceId]);

  // 帧率只统计这一段连续刷新
  useEffect(() => {
    arrivalsRef.current = [];
    setFpsView(null);
  }, [autoRefresh]);

  useEffect(() => {
    const pollStatus = createPollRunner(async (isCurrent) => {
      try {
        const next = await voiceApi.builtinStatus();
        if (mountedRef.current && isCurrent()) {
          setBuiltinStatus(next);
          setStatusError("");
        }
      } catch (err) {
        if (mountedRef.current && isCurrent()) {
          setStatusError(voiceErrorText(err));
        }
      }
    });
    void pollStatus();
    const timer = setInterval(() => {
      void pollStatus();
    }, CAMERA_STATUS_POLL_MS);
    return () => {
      pollStatus.invalidate();
      clearInterval(timer);
    };
  }, []);

  const isWaitingFrame = Boolean(capturing || frameMeta?.pending);

  useEffect(() => {
    if (!selectedDeviceId) {
      activeFramePollRef.current = null;
      return undefined;
    }
    const targetId = selectedDeviceId;
    const pollFrame = createPollRunner(async (isCurrent) => {
      try {
        const afterSeq = frameSeqRef.current;
        const next = await voiceApi.deviceRequest(
          targetId,
          "camera_frame",
          afterSeq != null ? { after_seq: afterSeq } : undefined,
        );
        if (!mountedRef.current || !isCurrent() || selectedDeviceRef.current !== targetId) {
          return;
        }
        const rawError = next?.last_error || null;
        const errorSeq = next?.error_seq ?? null;
        const nextError =
          rawError && errorSeq != null && errorSeq === dismissedErrorSeqRef.current
            ? null
            : rawError;
        if (nextError) {
          setAutoRefresh(false);
        }
        setFrameMeta({
          running: Boolean(next?.running),
          online: Boolean(next?.online),
          session_state: next?.session_state || null,
          has_camera_tool: next?.has_camera_tool ?? null,
          pending: Boolean(next?.pending),
          last_error: nextError,
          error_seq: errorSeq,
        });
        if (!next?.unchanged) {
          if (next?.frame) {
            if (frameSeqRef.current !== next.frame.seq) {
              arrivalsRef.current = [...arrivalsRef.current, Date.now()].slice(-CAMERA_FPS_WINDOW);
              setFpsView(cameraFpsView(arrivalsRef.current));
            }
            frameSeqRef.current = next.frame.seq;
            setFrame(next.frame);
          } else {
            frameSeqRef.current = null;
            setFrame(null);
          }
        }
      } catch (err) {
        if (mountedRef.current && isCurrent() && selectedDeviceRef.current === targetId) {
          setActionError(voiceErrorText(err));
        }
      }
    });
    activeFramePollRef.current = pollFrame;
    void pollFrame();
    const intervalMs = isWaitingFrame || autoRefresh ? CAMERA_FRAME_POLL_MS : 1500;
    const timer = setInterval(() => {
      void pollFrame();
    }, intervalMs);
    return () => {
      pollFrame.invalidate();
      if (activeFramePollRef.current === pollFrame) {
        activeFramePollRef.current = null;
      }
      clearInterval(timer);
    };
  }, [selectedDeviceId, isWaitingFrame, autoRefresh]);

  const triggerCapture = useCallback(
    async ({ silent = false } = {}) => {
      const targetId = selectedDeviceRef.current;
      if (!targetId) return false;
      const reqId = captureReqIdRef.current + 1;
      captureReqIdRef.current = reqId;
      activeFramePollRef.current?.invalidate?.();
      setCapturing(true);
      if (!silent) setActionError("");
      lastCaptureTriggeredAtRef.current = Date.now();
      try {
        await voiceApi.deviceRequest(targetId, "capture_photo");
        if (
          mountedRef.current &&
          selectedDeviceRef.current === targetId &&
          captureReqIdRef.current === reqId
        ) {
          activeFramePollRef.current?.invalidate?.();
          setFrameMeta((prev) => ({ ...(prev || {}), pending: true, last_error: null }));
          void activeFramePollRef.current?.();
        }
        return true;
      } catch (err) {
        if (
          mountedRef.current &&
          selectedDeviceRef.current === targetId &&
          captureReqIdRef.current === reqId
        ) {
          setActionError(voiceErrorText(err));
          setAutoRefresh(false);
        }
        return false;
      } finally {
        if (mountedRef.current && captureReqIdRef.current === reqId) {
          setCapturing(false);
        }
      }
    },
    [],
  );

  const autoGate = useMemo(
    () =>
      shouldAutoCaptureNext({
        autoEnabled: autoRefresh,
        device: activeDevice,
        frameView: frameMeta,
      }),
    [autoRefresh, activeDevice, frameMeta],
  );

  // 串行自动刷新：上一帧完成且满间隔、且小智空闲时才触发下一帧
  useEffect(() => {
    if (!autoRefresh || !autoGate.allow || capturing) return undefined;
    const elapsed = Date.now() - lastCaptureTriggeredAtRef.current;
    const waitMs = Math.max(CAMERA_AUTO_GAP_MS, CAMERA_AUTO_INTERVAL_MS - elapsed);
    const timer = setTimeout(() => {
      if (mountedRef.current) {
        void triggerCapture({ silent: true });
      }
    }, waitMs);
    return () => clearTimeout(timer);
  }, [autoRefresh, autoGate.allow, capturing, frame?.seq, triggerCapture]);

  const handleToggleAuto = useCallback(() => {
    if (autoRefresh) {
      setAutoRefresh(false);
      return;
    }
    dismissedErrorSeqRef.current = frameMeta?.error_seq ?? null;
    lastCaptureTriggeredAtRef.current = 0;
    setActionError("");
    setFrameMeta((meta) => (meta ? { ...meta, last_error: null } : meta));
    setAutoRefresh(true);
  }, [autoRefresh, frameMeta?.error_seq]);

  const handleClearFrame = useCallback(async () => {
    const targetId = selectedDeviceRef.current;
    if (!targetId) return;
    captureReqIdRef.current += 1;
    activeFramePollRef.current?.invalidate?.();
    dismissedErrorSeqRef.current = null;
    setCapturing(false);
    setAutoRefresh(false);
    setActionError("");
    try {
      await voiceApi.deviceRequest(targetId, "clear_camera_frame");
      if (mountedRef.current && selectedDeviceRef.current === targetId) {
        activeFramePollRef.current?.invalidate?.();
        frameSeqRef.current = null;
        setFrame(null);
        setFrameMeta((prev) =>
          prev ? { ...prev, pending: false, last_error: null, error_seq: null } : null,
        );
      }
    } catch (err) {
      if (mountedRef.current && selectedDeviceRef.current === targetId) {
        setActionError(voiceErrorText(err));
      }
    }
  }, []);

  const handleStartBuiltin = useCallback(async () => {
    setStartingServer(true);
    setActionError("");
    try {
      const next = await voiceApi.builtinStart();
      if (mountedRef.current) {
        setBuiltinStatus(next);
      }
    } catch (err) {
      if (mountedRef.current) {
        setActionError(voiceErrorText(err));
      }
    } finally {
      if (mountedRef.current) {
        setStartingServer(false);
      }
    }
  }, []);

  const updateViewPref = useCallback((key) => {
    setViewPrefs((prev) => {
      const next = { ...prev, [key]: !prev[key] };
      writeViewPrefs(next);
      return next;
    });
  }, []);

  const sourceBadge = cameraFrameSourceView(frame);
  const imgTransform = cameraTransformValue(viewPrefs);
  const displayError = actionError || frameMeta?.last_error || statusError;
  const effectiveCanCapture = Boolean(
    activeDevice?.canCapture && frameMeta?.has_camera_tool !== false,
  );
  const disabledHint =
    frameMeta?.has_camera_tool === false
      ? "该设备固件未上报摄像头工具（self.camera.take_photo）"
      : activeDevice?.disabledReason || "";

  // 2026-11-07 布局改版：去掉与顶栏重复的大标题和使用说明；左侧设备卡，右侧画面卡；
  // 抓拍 / 连续刷新放在画面上方工具条，镜像 / 旋转 / 清空收成图标按钮，画面信息叠在画面角上
  return (
    <div className="camera-page" id="camera-page">
      {!view.running && builtinStatus && (
        <section className="camera-banner ob-card" role="status" id="camera-stopped-banner">
          <div className="camera-banner-text">
            <AlertTriangle size={16} aria-hidden="true" />
            <span>内置语音服务没有运行，小智设备连不上本机，所以看不到画面。</span>
          </div>
          <button
            type="button"
            className="btn btn-hud-primary"
            id="camera-start-builtin-btn"
            disabled={startingServer}
            onClick={handleStartBuiltin}
          >
            {startingServer ? "正在启动…" : "启动内置语音服务"}
          </button>
        </section>
      )}

      {view.pendingCount > 0 && (
        <section className="camera-banner camera-banner--info ob-card" role="status">
          <div className="camera-banner-text">
            <Sparkles size={16} aria-hidden="true" />
            <span>有 {view.pendingCount} 台新的小智设备在等你允许，允许后才能看它的摄像头。</span>
          </div>
          {onNavigate && (
            <button type="button" className="btn btn-ghost" onClick={() => onNavigate("/voice")}>
              去允许设备
            </button>
          )}
        </section>
      )}

      <div className="camera-grid">
        <aside className="camera-sidebar hud-card ob-card" aria-label="摄像头设备">
          <div className="camera-section-head">
            <h2>设备</h2>
            <span className="ob-pill ob-pill--off">{view.devices.length} 台已允许</span>
          </div>
          {view.devices.length === 0 ? (
            <div className="camera-empty-devices" id="camera-no-devices">
              <p>还没有已允许的小智设备。</p>
              <small>把小智固件的 OTA 地址指向本机语音中心后开机，再到「语音中心 → 设备」点一次允许。</small>
            </div>
          ) : (
            <div className="camera-device-list" role="listbox" aria-label="已允许的小智设备">
              {view.devices.map((dev) => {
                const selected = dev.deviceId === selectedDeviceId;
                return (
                  <button
                    key={dev.deviceId}
                    type="button"
                    role="option"
                    aria-selected={selected}
                    className={`camera-device-item${selected ? " is-selected" : ""}`}
                    onClick={() => setSelectedDeviceId(dev.deviceId)}
                  >
                    <div className="camera-device-row">
                      <strong>{dev.title}</strong>
                      <span
                        className={`ob-pill ${
                          dev.canCapture ? TONE_CLASS.ok : dev.online ? TONE_CLASS.warn : TONE_CLASS.off
                        }`}
                      >
                        {dev.online ? dev.sessionStateLabel : "离线"}
                      </span>
                    </div>
                    <div className="camera-device-meta">
                      {dev.ip && <span>{dev.ip}</span>}
                      {dev.model && <span>{dev.model}</span>}
                      {dev.hasCameraTool === true && <span>支持拍照</span>}
                      {dev.hasCameraTool === false && <span>无摄像头工具</span>}
                    </div>
                  </button>
                );
              })}
            </div>
          )}

          <div className="camera-service-row">
            <span
              className={`ob-pill ${view.running ? TONE_CLASS.ok : TONE_CLASS.off}`}
              id="camera-service-pill"
            >
              <Radio size={12} aria-hidden="true" />
              <span>{view.running ? `内置语音运行中 · 端口 ${view.httpPort}` : "内置语音未启动"}</span>
            </span>
            {onNavigate && (
              <button
                type="button"
                className="btn btn-ghost camera-jump-voice"
                id="camera-open-voice-center"
                onClick={() => onNavigate("/voice")}
              >
                <span>语音中心</span>
                <ArrowUpRight size={13} aria-hidden="true" />
              </button>
            )}
          </div>
        </aside>

        <section className="camera-stage hud-card ob-card" aria-label="摄像头画面">
          <div className="camera-toolbar" role="toolbar" aria-label="画面控制">
            <div className="camera-toolbar-main">
              <button
                type="button"
                className="btn btn-hud-primary camera-capture-btn"
                id="camera-capture-btn"
                disabled={!effectiveCanCapture || isWaitingFrame}
                onClick={() => void triggerCapture({ silent: false })}
              >
                {isWaitingFrame ? (
                  <>
                    <RefreshCw size={15} className="spin" aria-hidden="true" />
                    <span>正在回传…</span>
                  </>
                ) : (
                  <>
                    <Camera size={15} aria-hidden="true" />
                    <span>抓拍</span>
                  </>
                )}
              </button>
              <button
                type="button"
                className={`btn ${autoRefresh ? "btn-hud-primary" : "btn-ghost"} camera-auto-btn`}
                id="camera-auto-toggle"
                aria-pressed={autoRefresh}
                disabled={!effectiveCanCapture && !autoRefresh}
                onClick={handleToggleAuto}
                title="每约 1.6 秒抓拍一张；你和小智说话时会自动暂停"
              >
                {autoRefresh ? <Pause size={14} aria-hidden="true" /> : <Play size={14} aria-hidden="true" />}
                <span>{autoRefresh ? "停止连续刷新" : "连续刷新"}</span>
              </button>
            </div>
            <div className="camera-toolbar-tools">
              <button
                type="button"
                className={`camera-icon-btn${viewPrefs.mirror ? " is-on" : ""}`}
                id="camera-mirror-btn"
                aria-pressed={viewPrefs.mirror}
                aria-label="水平镜像"
                title="水平镜像"
                onClick={() => updateViewPref("mirror")}
              >
                <FlipHorizontal2 size={15} aria-hidden="true" />
              </button>
              <button
                type="button"
                className={`camera-icon-btn${viewPrefs.flip180 ? " is-on" : ""}`}
                id="camera-flip-btn"
                aria-pressed={viewPrefs.flip180}
                aria-label="旋转 180°"
                title="旋转 180°（摄像头倒装时用）"
                onClick={() => updateViewPref("flip180")}
              >
                <RotateCw size={15} aria-hidden="true" />
              </button>
              <button
                type="button"
                className="camera-icon-btn"
                id="camera-clear-btn"
                aria-label="清空画面"
                title="清空画面"
                disabled={!frame && !displayError}
                onClick={handleClearFrame}
              >
                <Trash2 size={15} aria-hidden="true" />
              </button>
            </div>
          </div>

          {disabledHint && (
            <p className="camera-hint camera-hint--warn" id="camera-disabled-reason">
              {disabledHint}
            </p>
          )}
          {autoRefresh && autoGate.pausedReason && (
            <p className="camera-hint camera-hint--info" id="camera-auto-paused-reason">
              {autoGate.pausedReason}
            </p>
          )}
          {displayError && (
            <div className="camera-error" role="alert" id="camera-error-box">
              <AlertTriangle size={15} aria-hidden="true" />
              <span>{displayError}</span>
            </div>
          )}

          <div className="camera-viewport" id="camera-viewport">
            {frame?.data_url ? (
              <CameraFrame
                src={frame.data_url}
                alt={`${activeDevice?.title || "小智设备"} 摄像头画面`}
                transform={imgTransform}
              />
            ) : (
              <div className="camera-placeholder" id="camera-placeholder">
                <Camera size={36} aria-hidden="true" />
                <strong>
                  {isWaitingFrame
                    ? "正在等小智拍照并传回画面…"
                    : effectiveCanCapture
                      ? "点上方「抓拍」看当前画面"
                      : "暂无摄像头画面"}
                </strong>
                <span>
                  {effectiveCanCapture
                    ? "画面只在局域网内传回本机，不用模型额度；进入本页不会自动开摄像头。对小智说「拍一下看看」时，照片和回答也会显示在这里。"
                    : disabledHint || "请先连接带摄像头的小智设备（如立创实战派 ESP32-S3）。"}
                </span>
              </div>
            )}
            <div className="camera-overlay camera-overlay--top">
              <span className={`ob-pill ${TONE_CLASS[sourceBadge.tone] || TONE_CLASS.off}`} id="camera-source-pill">
                {sourceBadge.label}
              </span>
              {autoRefresh && (
                <span
                  className="ob-pill ob-pill--ok"
                  id="camera-live-pill"
                  title="小智固件只有拍照没有视频流：连续刷新是一张接一张抓拍，帧率取决于小智拍照和上传的速度"
                >
                  连续刷新中{fpsView ? ` · ${fpsView.text}` : ""}
                </span>
              )}
            </div>
            {frame && (
              <div className="camera-overlay camera-overlay--bottom">
                <span className="camera-meta-chip" id="camera-resolution">
                  {frame.width} × {frame.height}
                </span>
                <span className="camera-meta-chip">{formatBytes(frame.size_bytes || 0)}</span>
                <span className="camera-meta-chip">抓拍于 {formatCaptureTime(frame.captured_at)}</span>
              </div>
            )}
          </div>

          {frame?.source === "voice" && (frame.question || frame.summary) && (
            <div className="camera-voice-qa" id="camera-voice-qa">
              {frame.question && (
                <div className="camera-qa-row">
                  <span className="camera-qa-label">语音提问</span>
                  <p>{frame.question}</p>
                </div>
              )}
              {frame.summary && (
                <div className="camera-qa-row">
                  <span className="camera-qa-label">视觉回答</span>
                  <p>{frame.summary}</p>
                </div>
              )}
            </div>
          )}
        </section>
      </div>
    </div>
  );
}

export default CameraPage;
