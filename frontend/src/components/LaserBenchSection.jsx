import React, { useEffect, useRef, useState } from "react";
import { Check, Copy, FlaskConical } from "lucide-react";
import { FoldHeader } from "./FoldHeader.jsx";
import { getJson, postJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { isReadOnlyReferenceClient } from "../authSession.js";
import {
  DRY_RUN_CONFIRM_MESSAGE,
  DRY_RUN_DEFAULT_MAX_FEED,
  DRY_RUN_MAX_FEED,
  DRY_RUN_MIN_FEED,
  buildLaserBenchPrompt,
  formatDryRunRemaining,
  formatLaserBenchStatusLine,
} from "../laserBenchPrompt.js";

function extractDryRun(info) {
  if (!info || typeof info !== "object") return null;
  return {
    enabled: Boolean(info.laser_dry_run_enabled),
    max_feed_mm_min: Number(info.dry_run_max_feed_mm_min) || DRY_RUN_DEFAULT_MAX_FEED,
    effective_max_feed_mm_min: Number(info.laser_dry_run_effective_max_feed_mm_min) || 1000,
    remaining_seconds: Number(info.laser_dry_run_remaining_seconds) || 0,
    expires_at: info.laser_dry_run_expires_at ?? null,
  };
}

function broadcastDryRun(next) {
  if (typeof window !== "undefined" && typeof window.dispatchEvent === "function") {
    window.dispatchEvent(new CustomEvent("moss:dry-run-changed", { detail: next }));
  }
}

/**
 * LB5 / LD 激光雕刻机 AI 自主测试精简卡片（三项核心元素）：
 * 1. 一行状态（已连接/未连接 · 空跑开启/关闭）；
 * 2. 「允许 AI 真机空跑测试（不出光、限速）」开关 + 速度上限输入框 + 剩余时间（只读客户端隐藏开关，仅保留状态与复制按钮）；
 * 3. 「一键复制 AI 激光测试提示词」按钮。
 *
 * `embedded=true` 用于语音中心「记录与诊断 › AI 联调与测试」分组内的「激光雕刻机测试」子块。
 */
export function LaserBenchSection({
  canEdit = true,
  embedded = false,
  connectionStatus = null,
  initialDryRun = null,
  onDryRunChange = null,
} = {}) {
  return (
    <LaserBenchPanel
      canEdit={canEdit}
      embedded={embedded}
      connectionStatus={connectionStatus}
      initialDryRun={initialDryRun}
      onDryRunChange={onDryRunChange}
    />
  );
}

function LaserBenchPanel({
  canEdit = true,
  embedded = false,
  connectionStatus = null,
  initialDryRun = null,
  onDryRunChange = null,
}) {
  const [dryRun, setDryRun] = useState(
    () =>
      initialDryRun || {
        enabled: false,
        max_feed_mm_min: DRY_RUN_DEFAULT_MAX_FEED,
        remaining_seconds: 0,
      },
  );
  const [feedInput, setFeedInput] = useState(() =>
    String(initialDryRun?.max_feed_mm_min || DRY_RUN_DEFAULT_MAX_FEED),
  );
  const [serverCanEdit, setServerCanEdit] = useState(true);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const copyTimerRef = useRef(null);

  useEffect(() => () => {
    if (copyTimerRef.current) clearTimeout(copyTimerRef.current);
  }, []);

  const editable = !isReadOnlyReferenceClient() && canEdit !== false && serverCanEdit;

  useEffect(() => {
    if (!initialDryRun) return;
    setDryRun(initialDryRun);
    setFeedInput(String(initialDryRun.max_feed_mm_min || DRY_RUN_DEFAULT_MAX_FEED));
  }, [initialDryRun]);

  useEffect(() => {
    let alive = true;
    const refresh = async () => {
      const res = await getJson("/api/system/info");
      if (!alive || !(res.ok && res.data?.success)) return;
      const result = res.data.result;
      if (result && typeof result === "object" && "can_edit_operator_settings" in result) {
        setServerCanEdit(result.can_edit_operator_settings !== false);
      }
      const next = extractDryRun(result);
      if (next) {
        setDryRun(next);
        setFeedInput(String(next.max_feed_mm_min));
      }
    };
    void refresh();
    const timer = setInterval(() => {
      if (typeof document !== "undefined" && !document.hidden) void refresh();
    }, 10000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);

  const parseFeed = () => {
    const num = Math.round(Number(feedInput));
    if (!Number.isFinite(num) || num < DRY_RUN_MIN_FEED || num > DRY_RUN_MAX_FEED) {
      return null;
    }
    return num;
  };

  const applyDryRunResponse = (result) => {
    const next = extractDryRun(result);
    if (!next) return;
    setDryRun(next);
    setFeedInput(String(next.max_feed_mm_min));
    onDryRunChange?.(next);
    broadcastDryRun(next);
  };

  const connected = Boolean(connectionStatus?.connected);
  const canToggleDryRun = connected || Boolean(dryRun.enabled);

  const handleToggleDryRun = async (checked) => {
    if (!editable) return;
    if (checked && !connected) return;
    const feed = parseFeed();
    if (checked && feed === null) {
      await showAlert(`速度上限必须在 ${DRY_RUN_MIN_FEED} ~ ${DRY_RUN_MAX_FEED} mm/min 之间`, {
        title: "速度上限无效",
        tone: "warning",
      });
      return;
    }
    if (checked) {
      const ok = await showConfirm(DRY_RUN_CONFIRM_MESSAGE, {
        title: "开启真机空跑模式",
        confirmText: "开启空跑",
        cancelText: "取消",
        tone: "warning",
      });
      if (!ok) return;
    }
    setBusy(true);
    try {
      const payload = checked
        ? { laser_dry_run_enabled: true, dry_run_max_feed_mm_min: feed }
        : { laser_dry_run_enabled: false };
      const res = await postJson("/api/settings", payload);
      if (!(res.ok && res.data?.success)) {
        throw new Error(res.data?.result || res.error || "保存失败");
      }
      applyDryRunResponse(res.data.result);
    } catch (error) {
      await showAlert(String(error?.message ?? error), { title: "空跑模式设置失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const handleCommitFeed = async () => {
    if (!editable) return;
    const feed = parseFeed();
    if (feed === null) {
      setFeedInput(String(dryRun.max_feed_mm_min || DRY_RUN_DEFAULT_MAX_FEED));
      return;
    }
    if (feed === dryRun.max_feed_mm_min) return;
    setBusy(true);
    try {
      const res = await postJson("/api/settings", { dry_run_max_feed_mm_min: feed });
      if (!(res.ok && res.data?.success)) {
        throw new Error(res.data?.result || res.error || "保存失败");
      }
      applyDryRunResponse(res.data.result);
    } catch (error) {
      setFeedInput(String(dryRun.max_feed_mm_min || DRY_RUN_DEFAULT_MAX_FEED));
      await showAlert(String(error?.message ?? error), { title: "速度上限保存失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const handleCopyPrompt = async () => {
    const prompt = buildLaserBenchPrompt();
    try {
      await navigator.clipboard.writeText(prompt);
      setCopied(true);
      if (copyTimerRef.current) clearTimeout(copyTimerRef.current);
      copyTimerRef.current = setTimeout(() => setCopied(false), 2500);
    } catch {
      await showAlert("无法写入剪贴板，请检查系统剪贴板权限", { title: "复制失败", tone: "error" });
    }
  };

  const statusLine = formatLaserBenchStatusLine({
    connected,
    dryRun,
  });

  const laserHintText = !connected
    ? dryRun.enabled
      ? "当前未连接激光机，可随时取消勾选关闭空跑；重新连接后再跑真机空跑。"
      : "请先连接激光雕刻机后再开启真机空跑（未连接时仅可跑 L0 模拟机，不能进入真机测试）。"
    : "模拟机与只读核对不用开开关；跑真机空跑再勾选（不出光、60 分钟自动关）。";

  const previewFeed = parseFeed() ?? (Number(dryRun.max_feed_mm_min) || DRY_RUN_DEFAULT_MAX_FEED);
  const approxMmPerSec = Math.max(1, Math.round(previewFeed / 60));

  const toggleId = embedded ? "voice-laser-dry-run-toggle" : "settings-laser-dry-run-toggle";
  const feedId = embedded ? "voice-laser-dry-run-feed" : "settings-laser-dry-run-feed";
  const remainingId = embedded ? "voice-laser-dry-run-remaining" : "settings-laser-dry-run-remaining";
  const copyBtnId = embedded ? "voice-laser-bench-copy-btn" : "settings-laser-bench-copy-btn";

  const controlsRow = editable ? (
    <div className={embedded ? "ai-bench-laser-controls" : "settings-row"} style={embedded ? undefined : { flexWrap: "wrap", alignItems: "center", gap: "10px", marginBottom: "10px" }}>
      <label
        className={embedded ? "voice-check" : "settings-label"}
        style={embedded ? undefined : { marginBottom: 0 }}
        title={!canToggleDryRun ? "请先连接激光雕刻机后再开启真机空跑" : undefined}
      >
        <input
          type="checkbox"
          id={toggleId}
          checked={Boolean(dryRun.enabled)}
          disabled={busy || !canToggleDryRun}
          onChange={(e) => void handleToggleDryRun(e.target.checked)}
        />
        <span>允许 AI 真机空跑测试（不出光、限速）</span>
      </label>
      <label className={embedded ? "ai-bench-laser-feed" : undefined} style={embedded ? undefined : { display: "inline-flex", alignItems: "center", gap: "6px", fontSize: "12px" }}>
        <span>空跑限速</span>
        <input
          type="number"
          id={feedId}
          className="text-input"
          style={{ width: "86px", padding: "4px 8px" }}
          min={DRY_RUN_MIN_FEED}
          max={DRY_RUN_MAX_FEED}
          step={100}
          value={feedInput}
          disabled={busy}
          onChange={(e) => setFeedInput(e.target.value)}
          onBlur={() => void handleCommitFeed()}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void handleCommitFeed();
            }
          }}
          aria-label="空跑速度上限（mm/min）"
        />
        <span>mm/min</span>
        <span className="voice-muted ai-bench-laser-speed-equiv">（约 {approxMmPerSec} mm/s）</span>
      </label>
      {dryRun.enabled && (
        <span className="ob-pill ob-pill--warn" id={remainingId}>
          剩余 {formatDryRunRemaining(dryRun.remaining_seconds)}
        </span>
      )}
    </div>
  ) : null;

  const copyButton = (
    <button
      type="button"
      className={embedded ? "btn btn-primary" : "btn btn-hud-primary settings-btn"}
      id={copyBtnId}
      onClick={() => void handleCopyPrompt()}
    >
      {copied ? <Check size={13} /> : <Copy size={13} />}
      <span>{copied ? "已复制，直接发给 AI 即可" : "一键复制 AI 激光测试提示词"}</span>
    </button>
  );

  if (embedded) {
    return (
      <div
        className={`voice-block ai-bench-panel-card ai-bench-laser-block${dryRun.enabled ? " is-active" : ""}`}
        id="voice-laser-bench-box"
      >
        <div className="voice-block-h">
          <h3>
            <FlaskConical size={14} aria-hidden="true" className="ai-bench-laser-icon" />
            <span>激光雕刻机测试</span>
          </h3>
          <span className={`ob-pill ${dryRun.enabled ? "ob-pill--warn" : "ob-pill--off"}`} id="voice-laser-bench-status">
            {statusLine}
          </span>
        </div>
        {controlsRow}
        <div className="voice-actions ai-bench-laser-actions">
          {copyButton}
          <span className="voice-muted ai-bench-laser-hint" id="voice-laser-bench-hint">
            {laserHintText}
          </span>
        </div>
      </div>
    );
  }

  return (
    <div className="ob-fold settings-spacer" id="settings-laser-bench-section">
      <FoldHeader
        variant="section"
        id="settings-laser-bench"
        icon={FlaskConical}
        title="AI 激光自主测试"
        summary={statusLine}
        defaultOpen={false}
      />
      <div className="settings-box-4" id="settings-laser-bench-status">
        {statusLine}
      </div>
      {controlsRow}
      {copyButton}
    </div>
  );
}
