import React, { useEffect, useState } from "react";
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
 * LB5 偏好设置 → 诊断区「AI 激光自主测试」精简卡片（仅三项）：
 * 1. 一行状态（已连接/未连接 · 空跑开启/关闭）；
 * 2. 「允许 AI 真机空跑测试（不出光、限速）」开关 + 速度上限输入框 + 剩余时间；
 * 3. 「一键复制 AI 激光测试提示词」按钮。
 */
export function LaserBenchSection({
  canEdit = true,
  connectionStatus = null,
  initialDryRun = null,
  onDryRunChange = null,
} = {}) {
  if (isReadOnlyReferenceClient() || canEdit === false) return null;
  return (
    <LaserBenchPanel
      connectionStatus={connectionStatus}
      initialDryRun={initialDryRun}
      onDryRunChange={onDryRunChange}
    />
  );
}

function LaserBenchPanel({ connectionStatus = null, initialDryRun = null, onDryRunChange = null }) {
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
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);

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
      const next = extractDryRun(res.data.result);
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

  const handleToggleDryRun = async (checked) => {
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
    } catch {
      await showAlert("无法写入剪贴板，请检查系统剪贴板权限", { title: "复制失败", tone: "error" });
    }
  };

  const statusLine = formatLaserBenchStatusLine({
    connected: Boolean(connectionStatus?.connected),
    dryRun,
  });

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
      <div className="settings-row" style={{ flexWrap: "wrap", alignItems: "center", gap: "10px", marginBottom: "10px" }}>
        <label className="settings-label" style={{ marginBottom: 0 }}>
          <input
            type="checkbox"
            id="settings-laser-dry-run-toggle"
            checked={Boolean(dryRun.enabled)}
            disabled={busy}
            onChange={(e) => void handleToggleDryRun(e.target.checked)}
          />
          <span>允许 AI 真机空跑测试（不出光、限速）</span>
        </label>
        <label style={{ display: "inline-flex", alignItems: "center", gap: "6px", fontSize: "12px" }}>
          <span>上限</span>
          <input
            type="number"
            id="settings-laser-dry-run-feed"
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
        </label>
        {dryRun.enabled && (
          <span className="ob-pill ob-pill--warn" id="settings-laser-dry-run-remaining">
            剩余 {formatDryRunRemaining(dryRun.remaining_seconds)}
          </span>
        )}
      </div>
      <button
        type="button"
        className="btn btn-hud-primary settings-btn"
        id="settings-laser-bench-copy-btn"
        onClick={() => void handleCopyPrompt()}
      >
        {copied ? <Check size={13} /> : <Copy size={13} />}
        <span>{copied ? "已复制提示词" : "一键复制 AI 激光测试提示词"}</span>
      </button>
    </div>
  );
}
