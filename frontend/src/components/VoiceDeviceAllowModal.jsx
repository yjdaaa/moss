import React, { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  AlertTriangle,
  Check,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Copy,
  Loader2,
  Radio,
  ShieldCheck,
  ShieldOff,
  X,
} from "lucide-react";
import { showAlert, showConfirm } from "../appDialog.js";
import { isTopmostModalBackdrop } from "./AppDialog.jsx";
import { formatTime } from "./voice-builtin/shared.js";
import { createPollRunner } from "../pollGuard.js";
import { VOICE_BUILTIN_STATUS_EVENT, voiceApi } from "../voiceApi.js";
import {
  AUTO_ALLOW_DEVICES_CONFIRM_OPTIONS,
  AUTO_ALLOW_DEVICES_CONFIRM_TEXT,
  VOICE_POLL_MS,
  builtinDeviceView,
  voiceErrorText,
} from "../voiceState.js";

/** 允许成功后在弹窗内停留展示反馈的时长（毫秒）。 */
export const ALLOW_SUCCESS_FEEDBACK_MS = 1500;

/** C1 防冒认核对提示文案。 */
export const ANTI_SPOOF_PROMPT_TEXT = "请确认身边的小智屏幕正显示「等待允许」，再点允许。";

/** C5 允许成功主提示与补充说明（对齐 allowDeviceConfirmText 的常连模式说明与免重启识图限制）。 */
export const ALLOW_SUCCESS_HEADLINE = "已允许，现在唤醒小智即可对话";
export const ALLOW_SUCCESS_DETAIL =
  "若设备使用 MQTT 常连模式，需重启设备才能生效；免重启接通时，拍照识图要等设备下次重启后才可用。";

/**
 * 弹窗标题区主显示名（C2）：优先显示设备备注名，没有名字时显示型号，均缺省时回退通用名。
 */
export function voiceDeviceAllowHeading(device) {
  const name = String(device?.name ?? "").trim();
  if (name) return name;
  const model = String(device?.model ?? "").trim();
  if (model) return model;
  return "小智语音硬件";
}

/**
 * 将 Unix 秒时间戳格式化为中文相对时间（C2：用于“首次出现 / 最近来访”）。
 */
export function formatRelativeUnixSeconds(unixSeconds, nowSeconds = Date.now() / 1000) {
  const ts = Number(unixSeconds);
  const now = Number(nowSeconds);
  if (!Number.isFinite(ts) || ts <= 0 || !Number.isFinite(now)) return "—";
  const diff = Math.max(0, Math.floor(now - ts));
  if (diff < 10) return "刚刚";
  if (diff < 60) return `${diff} 秒前`;
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
  return `${Math.floor(diff / 86400)} 天前`;
}

/**
 * 从内置语音状态中挑出需要居中弹窗提示用户允许的待接入小智设备（n133）。
 * - 存量 `pending` 设备即使在 `auto_allow_devices` 开启后仍照常弹窗逐台确认（自动允许仅对之后新连入设备生效）；
 * - 若用户对某台设备点过“稍后处理”，在同一 `last_seen` 来访时间戳下不再重复弹出；
 *   当设备再次唤醒重连（`last_seen` 更新）时重新提示。
 */
export function selectPendingVoiceDevices(status, dismissedMap = new Map()) {
  if (!status || status.error) return [];
  const devices = Array.isArray(status.devices) ? status.devices : [];
  return devices.filter((device) => {
    if (!device?.device_id || builtinDeviceView(device).status !== "pending") return false;
    const dismissedAt = dismissedMap.get(device.device_id);
    if (dismissedAt === undefined) return true;
    const seenAt = Number(device.last_seen) || 0;
    return seenAt > dismissedAt;
  });
}

/**
 * 解析多台待允许设备队列的当前选中项与前后翻页可用性（C3）。
 */
export function resolvePendingDeviceQueue(pendingList, selectedDeviceId = "") {
  const list = Array.isArray(pendingList) ? pendingList : [];
  if (list.length === 0) {
    return { current: null, index: -1, total: 0, hasPrev: false, hasNext: false };
  }
  const foundIdx = selectedDeviceId
    ? list.findIndex((item) => item?.device_id === selectedDeviceId)
    : -1;
  const index = foundIdx >= 0 ? foundIdx : 0;
  return {
    current: list[index],
    index,
    total: list.length,
    hasPrev: index > 0,
    hasNext: index < list.length - 1,
  };
}

/** 在待允许队列中切换到上一台（delta=-1）或下一台（delta=1）。 */
export function stepPendingQueueDeviceId(pendingList, selectedDeviceId, delta) {
  const { index, total } = resolvePendingDeviceQueue(pendingList, selectedDeviceId);
  if (total === 0) return "";
  const nextIndex = Math.max(0, Math.min(total - 1, index + (Number(delta) || 0)));
  return pendingList[nextIndex]?.device_id || "";
}

/** 当前设备被允许、拒绝或稍后处理移出队列后，计算自动衔接显示的下一台设备 ID。 */
export function nextPendingDeviceIdAfterAction(pendingList, handledDeviceId) {
  const list = Array.isArray(pendingList) ? pendingList : [];
  const idx = list.findIndex((item) => item?.device_id === handledDeviceId);
  const rest = list.filter((item) => item?.device_id !== handledDeviceId);
  if (rest.length === 0) return "";
  if (idx >= 0 && idx < rest.length) return rest[idx].device_id;
  return rest[0].device_id;
}

/** 按 `device_id` 独立读取备注名草稿与自动允许勾选状态（C3：多台切换互不串台）。 */
export function readDeviceAllowDraft(draftMap, device) {
  const id = device?.device_id || "";
  const saved = id && draftMap && typeof draftMap === "object" ? draftMap[id] : null;
  if (saved) {
    return {
      nameDraft: String(saved.nameDraft ?? ""),
      autoAllowAll: Boolean(saved.autoAllowAll),
    };
  }
  return {
    nameDraft: String(device?.name ?? ""),
    autoAllowAll: false,
  };
}

/** 按 `device_id` 独立写入备注名草稿或自动允许勾选状态。 */
export function writeDeviceAllowDraft(draftMap, device, patch) {
  const id = device?.device_id || "";
  if (!id) return draftMap || {};
  const prev = readDeviceAllowDraft(draftMap, device);
  return {
    ...(draftMap || {}),
    [id]: {
      ...prev,
      ...patch,
    },
  };
}

function dropDeviceAllowDraft(draftMap, deviceId) {
  if (!deviceId || !draftMap || typeof draftMap !== "object" || !(deviceId in draftMap)) {
    return draftMap || {};
  }
  const next = { ...draftMap };
  delete next[deviceId];
  return next;
}

/**
 * 执行弹窗「允许并记住此设备」流程（独立导出便于单测确认门禁与分段报错）：
 * 1. 勾选了 `autoAllowAll` 时先弹局域网信任确认，取消则整体中止；
 * 2. 先把当前设备设为 `allowed`（失败报“允许设备失败”）；
 * 3. 允许成功后再执行备注改名与 `auto_allow_devices` 保存，任一失败单独提示“设备已允许，但备注名 / 自动允许开关保存失败”。
 */
export async function executeAllowVoiceDevice({
  device,
  nameDraft = "",
  autoAllowAll = false,
  api = voiceApi,
  confirm = showConfirm,
  alert = showAlert,
  onStart,
}) {
  if (autoAllowAll) {
    const confirmed = await confirm(
      AUTO_ALLOW_DEVICES_CONFIRM_TEXT,
      AUTO_ALLOW_DEVICES_CONFIRM_OPTIONS,
    );
    if (!confirmed) return { cancelled: true, allowed: false, status: null };
  }
  if (typeof onStart === "function") onStart();
  const targetId = device.device_id;
  const trimmedName = String(nameDraft ?? "").trim();
  const needRename = Boolean(trimmedName && trimmedName !== String(device.name ?? "").trim());
  let latestStatus;
  try {
    latestStatus = await api.deviceSetStatus(targetId, "allowed");
  } catch (error) {
    await alert(voiceErrorText(error), { title: "允许设备失败", tone: "error" });
    return { cancelled: false, allowed: false, status: null };
  }
  const failedItems = [];
  const errorDetails = [];
  if (needRename) {
    try {
      latestStatus = await api.deviceRename(targetId, trimmedName);
    } catch (error) {
      failedItems.push("备注名");
      errorDetails.push(voiceErrorText(error));
    }
  }
  if (autoAllowAll) {
    try {
      latestStatus = await api.localSave({ auto_allow_devices: true });
    } catch (error) {
      failedItems.push("自动允许开关");
      errorDetails.push(voiceErrorText(error));
    }
  }
  if (failedItems.length > 0) {
    const label = failedItems.join("和");
    await alert(`设备已允许，但${label}保存失败：${errorDetails.join("；")}`, {
      title: `设备已允许，但${label}保存失败`,
      tone: "warning",
    });
  }
  return { cancelled: false, allowed: true, status: latestStatus ?? null };
}

/**
 * 全局居中「发现小智设备请求连接」确认弹窗（2026-11-26 n133 用户决定）：
 * - 挂在 App 顶层，无论用户停留在雕刻工作台、创作画板还是语音中心，新小智首次连入（`pending`）时在屏幕正中央弹出；
 * - 支持多台排队切换（草稿按设备隔离）、防冒认核对提示、MAC 复制、允许成功 1.5s 状态反馈与焦点捕获还原。
 */
export function VoiceDeviceAllowModal({
  voiceEnabled = false,
  voiceStatus = null,
  currentPath = "/",
  initialStatus = null,
  initialSelectedId = "",
  initialDrafts = null,
  nowSeconds = null,
}) {
  const [builtinStatus, setBuiltinStatus] = useState(initialStatus);
  const [dismissVersion, setDismissVersion] = useState(0);
  const [selectedDeviceId, setSelectedDeviceId] = useState(initialSelectedId);
  const [draftsByDevice, setDraftsByDevice] = useState(() => initialDrafts || {});
  const [allowFeedback, setAllowFeedback] = useState(null);
  const [statusAnnouncement, setStatusAnnouncement] = useState("");
  const dismissedRef = useRef(new Map());
  const feedbackTimerRef = useRef(null);
  const statusClearTimerRef = useRef(null);

  useEffect(() => {
    return () => {
      if (feedbackTimerRef.current !== null) clearTimeout(feedbackTimerRef.current);
      if (statusClearTimerRef.current !== null) clearTimeout(statusClearTimerRef.current);
    };
  }, []);

  // 订阅全局内置语音状态事件（在 /voice 页面时直接复用 useBuiltinStatus 的轮询结果，零额外请求）
  useEffect(() => {
    if (!voiceEnabled || typeof window === "undefined") return undefined;
    const onStatus = (event) => {
      const next = event?.detail?.status;
      if (next && typeof next === "object" && Array.isArray(next.devices)) {
        setBuiltinStatus(next);
      }
    };
    window.addEventListener(VOICE_BUILTIN_STATUS_EVENT, onStatus);
    return () => window.removeEventListener(VOICE_BUILTIN_STATUS_EVENT, onStatus);
  }, [voiceEnabled]);

  // 不在 /voice 页面且内置语音服务处于启用/运行态时，低频轻量同步待允许设备列表
  const shouldPollBackground =
    voiceEnabled &&
    currentPath !== "/voice" &&
    (voiceStatus?.mode === "builtin" || voiceStatus?.device_sessions != null);

  useEffect(() => {
    if (!shouldPollBackground) return undefined;
    let cancelled = false;
    const sync = createPollRunner(async (isCurrent) => {
      try {
        const next = await voiceApi.builtinStatus({ lite: true });
        if (!cancelled && isCurrent() && next && !next.error) {
          setBuiltinStatus(next);
        }
      } catch {
        // 后台静默探测忽略瞬态错误
      }
    });
    void sync();
    const timer = setInterval(() => {
      if (!document.hidden) void sync();
    }, VOICE_POLL_MS);
    const onVisibility = () => {
      if (!document.hidden) void sync();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      cancelled = true;
      sync.invalidate();
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [shouldPollBackground]);

  void dismissVersion;
  const pendingList = selectPendingVoiceDevices(builtinStatus, dismissedRef.current);
  const queue = resolvePendingDeviceQueue(pendingList, selectedDeviceId);

  const announceStatus = useCallback((message) => {
    if (statusClearTimerRef.current !== null) clearTimeout(statusClearTimerRef.current);
    setStatusAnnouncement(message);
    if (message) {
      statusClearTimerRef.current = setTimeout(() => {
        statusClearTimerRef.current = null;
        setStatusAnnouncement("");
      }, 3000);
    }
  }, []);

  const finishAllowFeedback = useCallback((nextId) => {
    if (feedbackTimerRef.current !== null) {
      clearTimeout(feedbackTimerRef.current);
      feedbackTimerRef.current = null;
    }
    setAllowFeedback(null);
    if (typeof nextId === "string") {
      setSelectedDeviceId(nextId);
    }
  }, []);

  if (!voiceEnabled) return null;

  const modalOpen = Boolean(queue.current || allowFeedback);
  if (!modalOpen) {
    return statusAnnouncement ? (
      <div className="voice-device-allow-sr-status" role="status" aria-live="polite">
        {statusAnnouncement}
      </div>
    ) : null;
  }

  return (
    <VoiceDeviceAllowModalDialog
      builtinStatus={builtinStatus}
      setBuiltinStatus={setBuiltinStatus}
      pendingList={pendingList}
      queue={queue}
      selectedDeviceId={selectedDeviceId}
      setSelectedDeviceId={setSelectedDeviceId}
      draftsByDevice={draftsByDevice}
      setDraftsByDevice={setDraftsByDevice}
      dismissedRef={dismissedRef}
      bumpDismissVersion={() => setDismissVersion((v) => v + 1)}
      allowFeedback={allowFeedback}
      setAllowFeedback={setAllowFeedback}
      feedbackTimerRef={feedbackTimerRef}
      finishAllowFeedback={finishAllowFeedback}
      statusAnnouncement={statusAnnouncement}
      announceStatus={announceStatus}
      nowSeconds={nowSeconds}
    />
  );
}

function VoiceDeviceAllowModalDialog({
  builtinStatus,
  setBuiltinStatus,
  pendingList,
  queue,
  setSelectedDeviceId,
  draftsByDevice,
  setDraftsByDevice,
  dismissedRef,
  bumpDismissVersion,
  allowFeedback,
  setAllowFeedback,
  feedbackTimerRef,
  finishAllowFeedback,
  statusAnnouncement,
  announceStatus,
  nowSeconds,
}) {
  const [busyAction, setBusyAction] = useState("");
  const [copiedId, setCopiedId] = useState("");
  const [, setClockTick] = useState(0);
  const backdropRef = useRef(null);
  const dialogRef = useRef(null);
  const nameInputRef = useRef(null);
  const backdropPressRef = useRef(false);
  const mountedRef = useRef(true);
  const copyTimerRef = useRef(null);

  const busy = Boolean(busyAction);
  const activeDevice = allowFeedback?.device || queue.current;
  const activeDraft = readDeviceAllowDraft(draftsByDevice, activeDevice);
  const alreadyAutoAllow = builtinStatus?.local?.settings?.auto_allow_devices === true;

  // 打开弹窗时捕获原焦点并把初始焦点放到备注名输入框（不落在允许按钮上防回车误触），关闭卸载时还原原焦点
  useEffect(() => {
    mountedRef.current = true;
    const previousFocus = typeof document !== "undefined" ? document.activeElement : null;
    const focusTimer = typeof window !== "undefined"
      ? window.setTimeout(() => {
          nameInputRef.current?.focus();
        }, 0)
      : null;
    return () => {
      mountedRef.current = false;
      if (focusTimer !== null) window.clearTimeout(focusTimer);
      if (copyTimerRef.current !== null) clearTimeout(copyTimerRef.current);
      if (previousFocus && typeof previousFocus.focus === "function" && document.contains(previousFocus)) {
        previousFocus.focus();
      }
    };
  }, []);

  // 每 15 秒刷新一次相对时间显示（SSR 或传入固定 nowSeconds 时不启动定时器）
  useEffect(() => {
    if (nowSeconds !== null || typeof window === "undefined") return undefined;
    const timer = window.setInterval(() => setClockTick((v) => v + 1), 15000);
    return () => window.clearInterval(timer);
  }, [nowSeconds]);

  const dismissCurrent = useCallback(() => {
    if (busy) return;
    if (allowFeedback) {
      const nextId = allowFeedback.nextDeviceId || pendingList[0]?.device_id || "";
      finishAllowFeedback(nextId);
      if (nextId && typeof window !== "undefined") {
        window.setTimeout(() => {
          if (mountedRef.current) nameInputRef.current?.focus();
        }, 0);
      }
      return;
    }
    if (!queue.current) return;
    const targetId = queue.current.device_id;
    const nextId = nextPendingDeviceIdAfterAction(pendingList, targetId);
    dismissedRef.current.set(targetId, Number(queue.current.last_seen) || 0);
    setSelectedDeviceId(nextId);
    bumpDismissVersion();
  }, [busy, allowFeedback, finishAllowFeedback, queue.current, pendingList, dismissedRef, setSelectedDeviceId, bumpDismissVersion]);

  useEffect(() => {
    const onKey = (e) => {
      if (e.defaultPrevented || e.isComposing || e.keyCode === 229) return;
      if (e.key === "Escape") {
        if (!isTopmostModalBackdrop(backdropRef.current)) return;
        e.preventDefault();
        if (busy) return;
        dismissCurrent();
        return;
      }
      if (e.key === "Tab" && dialogRef.current && isTopmostModalBackdrop(backdropRef.current)) {
        const focusables = Array.from(
          dialogRef.current.querySelectorAll(
            'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
          ),
        );
        if (focusables.length === 0) return;
        const first = focusables[0];
        const last = focusables[focusables.length - 1];
        if (!dialogRef.current.contains(document.activeElement)) {
          e.preventDefault();
          (e.shiftKey ? last : first).focus();
        } else if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, dismissCurrent]);

  if (!activeDevice) return null;

  const focusNameSoon = () => {
    if (typeof window !== "undefined") {
      window.setTimeout(() => {
        if (mountedRef.current) nameInputRef.current?.focus();
      }, 0);
    }
  };

  const handleCopyMac = async () => {
    const mac = String(activeDevice.device_id || "");
    if (!mac) return;
    try {
      await globalThis.navigator?.clipboard?.writeText?.(mac);
      if (!mountedRef.current) return;
      setCopiedId(mac);
      if (copyTimerRef.current !== null) clearTimeout(copyTimerRef.current);
      copyTimerRef.current = setTimeout(() => {
        copyTimerRef.current = null;
        if (mountedRef.current) setCopiedId("");
      }, 1800);
    } catch {
      // 剪贴板不可用时静默忽略，原等宽 MAC 仍可选中复制
    }
  };

  const handleAllow = async () => {
    if (!queue.current || busy || allowFeedback) return;
    const targetDevice = queue.current;
    const targetId = targetDevice.device_id;
    const nextId = nextPendingDeviceIdAfterAction(pendingList, targetId);
    const trimmedDraftName = activeDraft.nameDraft.trim();
    try {
      const result = await executeAllowVoiceDevice({
        device: targetDevice,
        nameDraft: activeDraft.nameDraft,
        autoAllowAll: !alreadyAutoAllow && activeDraft.autoAllowAll,
        onStart: () => setBusyAction("allow"),
      });
      if (!mountedRef.current || result.cancelled) return;
      if (result.allowed) {
        dismissedRef.current.delete(targetId);
        setDraftsByDevice((prev) => dropDeviceAllowDraft(prev, targetId));
        if (result.status && typeof result.status === "object") {
          setBuiltinStatus(result.status);
        }
        announceStatus("");
        const displayDevice = trimmedDraftName
          ? { ...targetDevice, name: trimmedDraftName }
          : targetDevice;
        setAllowFeedback({ device: displayDevice, nextDeviceId: nextId });
        if (feedbackTimerRef.current !== null) clearTimeout(feedbackTimerRef.current);
        feedbackTimerRef.current = setTimeout(() => {
          feedbackTimerRef.current = null;
          setAllowFeedback(null);
          setSelectedDeviceId(nextId);
          focusNameSoon();
        }, ALLOW_SUCCESS_FEEDBACK_MS);
      }
    } finally {
      if (mountedRef.current) setBusyAction("");
    }
  };

  const handleReject = async () => {
    if (!queue.current || busy || allowFeedback) return;
    const targetDevice = queue.current;
    const targetId = targetDevice.device_id;
    const targetTitle = voiceDeviceAllowHeading(targetDevice);
    const nextId = nextPendingDeviceIdAfterAction(pendingList, targetId);
    setBusyAction("reject");
    try {
      const next = await voiceApi.deviceSetStatus(targetId, "revoked");
      if (!mountedRef.current) return;
      dismissedRef.current.delete(targetId);
      setDraftsByDevice((prev) => dropDeviceAllowDraft(prev, targetId));
      setSelectedDeviceId(nextId);
      announceStatus(`已拒绝设备「${targetTitle}」（${targetId}）接入`);
      if (next && typeof next === "object") setBuiltinStatus(next);
      if (nextId) focusNameSoon();
    } catch (error) {
      if (mountedRef.current) {
        await showAlert(voiceErrorText(error), { title: "拒绝设备失败", tone: "error" });
      }
    } finally {
      if (mountedRef.current) setBusyAction("");
    }
  };

  const effectiveNow = nowSeconds ?? Date.now() / 1000;
  const headingText = voiceDeviceAllowHeading(activeDevice);
  const modelSummary =
    [activeDevice.model, activeDevice.firmware_version && `固件 ${activeDevice.firmware_version}`]
      .filter(Boolean)
      .join(" · ") || "小智语音硬件";
  const firstSeenText = formatRelativeUnixSeconds(activeDevice.first_seen, effectiveNow);
  const lastSeenText = formatRelativeUnixSeconds(activeDevice.last_seen, effectiveNow);
  const seenTooltip = [
    activeDevice.first_seen ? `首次出现：${formatTime(activeDevice.first_seen)}` : "",
    activeDevice.last_seen ? `最近来访：${formatTime(activeDevice.last_seen)}` : "",
  ]
    .filter(Boolean)
    .join(" · ");
  const isCopied = copiedId === activeDevice.device_id;
  const hasNextAfterFeedback = Boolean(
    allowFeedback && (allowFeedback.nextDeviceId || pendingList.length > 0),
  );
  const dismissLabel = allowFeedback ? (hasNextAfterFeedback ? "下一台" : "关闭") : "稍后处理";

  const content = (
    <div
      ref={backdropRef}
      className="mat-modal-backdrop approval-backdrop voice-device-allow-backdrop"
      id="voice-device-allow-backdrop"
      onMouseDown={(e) => {
        backdropPressRef.current = e.button === 0 && e.target === e.currentTarget;
      }}
      onMouseUp={(e) => {
        if (e.target !== e.currentTarget) backdropPressRef.current = false;
      }}
      onClick={(e) => {
        if (backdropPressRef.current && e.target === e.currentTarget && !busy) {
          dismissCurrent();
        }
        backdropPressRef.current = false;
      }}
    >
      <div
        ref={dialogRef}
        className="mat-modal approval-card voice-device-allow-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="voice-device-allow-title"
        aria-describedby={
          allowFeedback
            ? "voice-device-allow-status"
            : "voice-device-allow-verify-tip voice-device-allow-desc"
        }
        id="voice-device-allow-modal"
      >
        <div className="mat-modal-head voice-device-allow-head">
          <div className="voice-device-allow-head-main">
            <h3 id="voice-device-allow-title" className="approval-title">
              <Radio size={18} aria-hidden="true" />
              <span id="voice-device-allow-heading">{headingText}</span>
            </h3>
            <p className="voice-device-allow-subtitle">发现小智设备请求连接</p>
          </div>
          <span
            className={`ob-pill voice-device-allow-pill ${allowFeedback ? "ob-pill--ok" : "ob-pill--warn"}`}
          >
            {allowFeedback ? "已允许" : "待允许"}
          </span>
          <button
            type="button"
            className="mat-modal-close voice-device-allow-close"
            id="voice-device-allow-close"
            onClick={dismissCurrent}
            disabled={busy}
            title={dismissLabel}
            aria-label={dismissLabel}
          >
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body voice-stack">
          {!allowFeedback && queue.total > 1 && (
            <div className="voice-device-allow-queue" id="voice-device-allow-queue">
              <span className="voice-device-allow-queue-label">
                第 <b className="mono">{queue.index + 1}</b> / <b className="mono">{queue.total}</b> 台待允许设备
              </span>
              <div className="voice-device-allow-queue-nav">
                <button
                  type="button"
                  className="btn btn-ghost voice-device-allow-queue-btn"
                  id="voice-device-allow-prev"
                  disabled={busy || !queue.hasPrev}
                  onClick={() =>
                    setSelectedDeviceId(stepPendingQueueDeviceId(pendingList, activeDevice.device_id, -1))
                  }
                >
                  <ChevronLeft size={14} aria-hidden="true" />
                  <span>上一台</span>
                </button>
                <button
                  type="button"
                  className="btn btn-ghost voice-device-allow-queue-btn"
                  id="voice-device-allow-next"
                  disabled={busy || !queue.hasNext}
                  onClick={() =>
                    setSelectedDeviceId(stepPendingQueueDeviceId(pendingList, activeDevice.device_id, 1))
                  }
                >
                  <span>下一台</span>
                  <ChevronRight size={14} aria-hidden="true" />
                </button>
              </div>
            </div>
          )}

          {allowFeedback ? (
            <div
              className="voice-hint voice-hint--ok voice-device-allow-status"
              id="voice-device-allow-status"
              role="status"
              aria-live="polite"
            >
              <CheckCircle2 size={16} aria-hidden="true" />
              <div className="voice-device-allow-status-body">
                <b>{ALLOW_SUCCESS_HEADLINE}</b>
                <span>{ALLOW_SUCCESS_DETAIL}</span>
              </div>
            </div>
          ) : statusAnnouncement ? (
            <div
              className="voice-hint voice-device-allow-status"
              id="voice-device-allow-status"
              role="status"
              aria-live="polite"
            >
              <span>{statusAnnouncement}</span>
            </div>
          ) : null}

          {!allowFeedback && (
            <div
              className="voice-hint voice-hint--warn voice-device-allow-verify"
              id="voice-device-allow-verify-tip"
            >
              <AlertTriangle size={15} aria-hidden="true" />
              <span>{ANTI_SPOOF_PROMPT_TEXT}</span>
            </div>
          )}

          <dl className="approval-kv voice-device-allow-kv">
            <dt>设备编号</dt>
            <dd className="voice-device-allow-mac-row">
              <code className="mono" id="voice-device-allow-mac">
                {activeDevice.device_id}
              </code>
              <button
                type="button"
                className="btn btn-ghost voice-device-allow-copy"
                id="voice-device-allow-copy-mac"
                onClick={handleCopyMac}
                aria-label={`复制设备编号 ${activeDevice.device_id}`}
                title="复制设备编号"
              >
                {isCopied ? <Check size={12} aria-hidden="true" /> : <Copy size={12} aria-hidden="true" />}
                <span>{isCopied ? "已复制" : "复制"}</span>
              </button>
            </dd>
            <dt>型号 / 固件</dt>
            <dd>{modelSummary}</dd>
            {activeDevice.last_ip && (
              <>
                <dt>局域网地址</dt>
                <dd className="mono">{activeDevice.last_ip}</dd>
              </>
            )}
            <dt>首次出现 / 最近来访</dt>
            <dd id="voice-device-allow-seen" title={seenTooltip || undefined}>
              {firstSeenText} / {lastSeenText}
            </dd>
          </dl>

          {!allowFeedback && (
            <>
              <div className="voice-device-allow-field">
                <label htmlFor="voice-device-allow-name" className="voice-device-allow-label">
                  设备备注名称（可选，方便区分多台小智）
                </label>
                <input
                  ref={nameInputRef}
                  type="text"
                  className="text-input voice-device-allow-input"
                  id="voice-device-allow-name"
                  maxLength={32}
                  value={activeDraft.nameDraft}
                  disabled={busy}
                  placeholder="例如：工作台小智 / 书桌小智"
                  onChange={(e) => {
                    const val = e.target.value;
                    setDraftsByDevice((prev) =>
                      writeDeviceAllowDraft(prev, activeDevice, { nameDraft: val }),
                    );
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.isComposing && e.keyCode !== 229) {
                      e.preventDefault();
                    }
                  }}
                />
              </div>

              {!alreadyAutoAllow && (
                <label className="voice-check voice-device-allow-check" id="voice-device-allow-auto-label">
                  <input
                    type="checkbox"
                    id="voice-device-allow-auto-all"
                    checked={activeDraft.autoAllowAll}
                    disabled={busy}
                    onChange={(e) => {
                      const checked = e.target.checked;
                      setDraftsByDevice((prev) =>
                        writeDeviceAllowDraft(prev, activeDevice, { autoAllowAll: checked }),
                      );
                    }}
                  />
                  <span className="voice-device-allow-check-text" id="voice-device-allow-auto-text">
                    以后自动允许局域网内新小智设备连接（免弹窗，仅建议在家庭或工作室网络开启）
                  </span>
                </label>
              )}

              <p className="voice-muted" id="voice-device-allow-desc" style={{ margin: 0 }}>
                允许后将永久记住这台设备，直接唤醒小智即可对话（常连模式需重启设备生效）；免重启接通时，拍照识图要等设备下次重启后才可用。
              </p>
            </>
          )}
        </div>

        <div className="mat-modal-foot voice-device-allow-foot">
          <button
            type="button"
            className="btn btn-ghost"
            id="voice-device-allow-later"
            onClick={dismissCurrent}
            disabled={busy}
          >
            {dismissLabel}
          </button>
          {!allowFeedback && (
            <button
              type="button"
              className="btn voice-device-allow-reject"
              id="voice-device-allow-reject"
              onClick={handleReject}
              disabled={busy}
              aria-busy={busyAction === "reject" || undefined}
            >
              {busyAction === "reject" ? (
                <>
                  <Loader2 size={14} className="spin" aria-hidden="true" />
                  <span>正在拒绝…</span>
                </>
              ) : (
                <>
                  <ShieldOff size={14} aria-hidden="true" />
                  <span>拒绝接入</span>
                </>
              )}
            </button>
          )}
          <button
            type="button"
            className="btn btn-primary"
            id="voice-device-allow-confirm"
            onClick={handleAllow}
            disabled={busy || Boolean(allowFeedback)}
            aria-busy={busyAction === "allow" || undefined}
          >
            {allowFeedback ? (
              <>
                <Check size={14} aria-hidden="true" />
                <span>已允许</span>
              </>
            ) : busyAction === "allow" ? (
              <>
                <Loader2 size={14} className="spin" aria-hidden="true" />
                <span>正在允许…</span>
              </>
            ) : (
              <>
                <ShieldCheck size={14} aria-hidden="true" />
                <span>允许并记住此设备</span>
              </>
            )}
          </button>
        </div>
      </div>
    </div>
  );

  return typeof document !== "undefined" && document.body
    ? createPortal(content, document.body)
    : content;
}
