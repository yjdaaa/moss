import React, { useEffect, useMemo, useRef, useState } from "react";
import { Excalidraw, exportToBlob } from "@excalidraw/excalidraw";
import "@excalidraw/excalidraw/index.css";
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  Clock,
  Download,
  Eye,
  FileJson,
  Home,
  RefreshCw,
  Ruler,
  Save,
  Send,
  Settings2,
  ShieldAlert,
  Square,
  UploadCloud,
  Wifi,
  X,
} from "lucide-react";
import { prepareRasterExportElements } from "../rasterInkElements.js";
import {
  sendBadgeLabel,
  summaryAllowsConfirmSend,
  summaryDisplayWarnings,
  thicknessDisplayRows,
} from "../jobSummaryView.js";
import {
  CUSTOM_OPTION_LABEL,
  CUSTOM_OPTION_VALUE,
  applyMaterialLibraryRefresh,
  applyMaterialSelectChange,
  findMaterialOption,
  loadUiMaterialOptions,
  restoreMaterialSelectState,
  returnToLibrary,
} from "../materialSelectState.js";
import { getLaserExportDimensions } from "../laserExportDimensions.js";
import {
  applyConfirmSendResponse,
  applyPreviewFingerprintSuccess,
  canSubmitConfirmSend,
  createRepeatSendState,
  requiresMovedMaterialAcknowledgement,
  setMovedMaterialAcknowledged,
} from "../repeatSendGuard.js";
import { isRemoteMode, resolveApiUrl } from "../authSession.js";
import { apiFetch, fetchPreviewBlobUrl, postJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { exportFile, savedMessage } from "../saveFile.js";
import { FeedbackPanel } from "../components/FeedbackPanel.jsx";
import { estopTargetArgs } from "../globalEstop.js";
import { usePageEmergencyStop } from "../usePageEstop.js";
import { FoldHeader } from "../components/FoldHeader.jsx";
import { WbCard, WorkbenchCanvas } from "../components/WorkbenchCanvas.jsx";
import { DRAW_DEFAULT_LAYOUT, DRAW_STORAGE_KEY } from "../workbenchLayout.js";
import {
  drawConnectionPayload,
  drawSizeValue,
  drawTargetLabel,
  drawTargetMissingText,
  drawTargetReady,
  formatFramingStatus,
  framingConfirmText,
  isSerial,
  readDrawDefaultForm,
  safeBrowserStorage,
} from "../drawConnection.js";

const CONFIG_KEYS = [
  "material",
  "thickness_mm",
  "width_mm",
  "height_mm",
  "mode",
  "connection_mode",
  "network_host",
  "network_telnet_port",
  "port",
  "baudrate",
  "vector_simplify_factor",
];
const EXCALIDRAW_LANG_CODE = "zh-CN";
const MATERIAL_CHANNEL_NAME = "laser-material-library-v1";

// 存储被禁用时逐项退回默认值，不能让模块顶层读取抛错导致整个应用打不开（2026-11-05）
const DEFAULT_FORM = readDrawDefaultForm();

export function DrawPage({ onNavigate }) {
  const [excalidrawAPI, setExcalidrawAPI] = useState(null);
  const [form, setForm] = useState(DEFAULT_FORM);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState("准备就绪");
  const [workflowId, setWorkflowId] = useState("");
  const [result, setResult] = useState(null);
  const [lastScene, setLastScene] = useState(null);
  const [connectionBusy, setConnectionBusy] = useState(false);
  const [connectionResult, setConnectionResult] = useState(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [materialLibrary, setMaterialLibrary] = useState([]);
  const [materialMode, setMaterialMode] = useState("custom");
  const [materialSelectValue, setMaterialSelectValue] = useState(CUSTOM_OPTION_VALUE);
  const [materialRefreshBusy, setMaterialRefreshBusy] = useState(false);
  const [repeatSend, setRepeatSend] = useState(() => createRepeatSendState(safeBrowserStorage()));
  const [serialPorts, setSerialPorts] = useState([]);
  const [framingLight, setFramingLight] = useState(false);
  const materialLibraryRef = useRef(materialLibrary);
  const materialModeRef = useRef(materialMode);
  const formRef = useRef(form);
  const materialLoadedOnceRef = useRef(false);
  const materialRefreshGenerationRef = useRef(0);
  const boardStageRef = useRef(null);

  useEffect(() => {
    const stage = boardStageRef.current;
    if (!stage || !excalidrawAPI || typeof ResizeObserver === "undefined") return undefined;
    const observer = new ResizeObserver(() => {
      excalidrawAPI.refresh?.();
    });
    observer.observe(stage);
    return () => observer.disconnect();
  }, [excalidrawAPI]);

  useEffect(() => {
    materialLibraryRef.current = materialLibrary;
  }, [materialLibrary]);
  useEffect(() => {
    materialModeRef.current = materialMode;
  }, [materialMode]);
  useEffect(() => {
    formRef.current = form;
  }, [form]);

  const web = result?.result?.web || {};
  const lab = result?.result?.draw_lab || {};
  const jobSummary = lab.job_summary || null;
  const rawText = useMemo(() => (result ? JSON.stringify(result, null, 2) : ""), [result]);
  const canActOnWorkflow = Boolean(workflowId);
  const hostReady = drawTargetReady(form);
  const nextActions = Array.isArray(result?.result?.next_actions) ? result.result.next_actions : [];
  const summaryAllowsSend = summaryAllowsConfirmSend(jobSummary, nextActions);
  const canConfirmSend = canActOnWorkflow && hostReady && summaryAllowsSend;

  function updateForm(key, value) {
    setForm((current) => ({ ...current, [key]: value }));
  }

  async function refreshMaterialLibrary() {
    const generation = ++materialRefreshGenerationRef.current;
    setMaterialRefreshBusy(true);
    try {
      const currentForm = formRef.current;
      const loaded = await loadUiMaterialOptions(apiFetch);
      if (generation !== materialRefreshGenerationRef.current) return;
      const decision = applyMaterialLibraryRefresh({
        loaded,
        currentMode: materialModeRef.current,
        currentMaterial: currentForm.material,
        currentThickness: currentForm.thickness_mm,
        previousMaterials: materialLibraryRef.current,
        hasLoadedOnce: materialLoadedOnceRef.current,
      });
      if (!decision.applied) {
        return;
      }
      if (loaded.ok) {
        materialLoadedOnceRef.current = true;
      }
      setMaterialLibrary(decision.materials);
      setMaterialMode(decision.mode);
      setMaterialSelectValue(decision.selectValue);
      const nextForm = {
        ...currentForm,
        material: decision.material || currentForm.material,
        thickness_mm: decision.thickness_mm || currentForm.thickness_mm,
      };
      setForm((current) => ({
        ...current,
        material: nextForm.material,
        thickness_mm: nextForm.thickness_mm,
      }));
    } finally {
      if (generation === materialRefreshGenerationRef.current) {
        setMaterialRefreshBusy(false);
      }
    }
  }

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const generation = ++materialRefreshGenerationRef.current;
      const loaded = await loadUiMaterialOptions(apiFetch);
      if (cancelled || generation !== materialRefreshGenerationRef.current) return;
      const restored = restoreMaterialSelectState({
        materials: loaded.materials,
        savedMaterial: form.material,
        savedThickness: form.thickness_mm,
      });
      if (loaded.ok) {
        materialLoadedOnceRef.current = true;
      }
      setMaterialLibrary(restored.materials);
      setMaterialMode(restored.mode);
      setMaterialSelectValue(restored.selectValue);
      const nextForm = {
        ...form,
        material: restored.material || form.material,
        thickness_mm: restored.thickness_mm || form.thickness_mm,
      };
      setForm((current) => ({
        ...current,
        material: nextForm.material,
        thickness_mm: nextForm.thickness_mm,
      }));
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    function onFocus() {
      refreshMaterialLibrary();
    }
    function onVisibility() {
      if (document.visibilityState === "visible") {
        refreshMaterialLibrary();
      }
    }
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    let channel = null;
    try {
      if (typeof BroadcastChannel === "function") {
        channel = new BroadcastChannel(MATERIAL_CHANNEL_NAME);
        channel.addEventListener("message", (event) => {
          if (event && event.data && event.data.type === "materials-updated") {
            refreshMaterialLibrary();
          }
        });
      }
    } catch (_error) {
      channel = null;
    }
    return () => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
      if (channel) {
        try {
          channel.close();
        } catch (_error) {
          // ignore close failures
        }
      }
    };
  }, []);

  function handleMaterialSelectChange(selectValue) {
    const next = applyMaterialSelectChange({
      materials: materialLibrary,
      currentThickness: form.thickness_mm,
      selectValue,
      customMaterial: form.material,
    });
    setMaterialLibrary(next.materials);
    setMaterialMode(next.mode);
    setMaterialSelectValue(next.selectValue);
    setForm((current) => ({
      ...current,
      material: next.material,
      thickness_mm: next.thickness_mm,
    }));
  }

  function handleThicknessSelectChange(value) {
    updateForm("thickness_mm", value);
  }

  function handleReturnToLibrary() {
    const next = returnToLibrary({
      materials: materialLibrary,
      preferredMaterial: form.material,
      preferredThickness: form.thickness_mm,
    });
    setMaterialLibrary(next.materials);
    setMaterialMode(next.mode);
    setMaterialSelectValue(next.selectValue);
    setForm((current) => ({
      ...current,
      material: next.material,
      thickness_mm: next.thickness_mm,
    }));
  }

  function saveConfiguration() {
    try {
      const storage = safeBrowserStorage();
      if (!storage) throw new Error("浏览器存储不可用");
      CONFIG_KEYS.forEach((key) => {
        const value = form[key];
        storage.setItem(`laserLab.${key}`, typeof value === "boolean" ? (value ? "true" : "false") : (value ?? ""));
      });
      setStatus("配置已保存");
    } catch (error) {
      setStatus(`配置保存失败：${error?.message || error}（本次设置仍可使用，但不会被记住）`);
    }
  }

  async function exportScene({ forLaser = false } = {}) {
    if (!excalidrawAPI) {
      throw new Error("画板还没有加载完成");
    }
    const elements = excalidrawAPI.getSceneElements();
    const activeElements = elements.filter((element) => !element.isDeleted);
    if (activeElements.length === 0) {
      throw new Error("画板是空的");
    }
    const appState = excalidrawAPI.getAppState();
    const files = excalidrawAPI.getFiles();
    const exportElements = forLaser ? prepareRasterExportElements(activeElements, form.mode) : activeElements;
    const exportAppState = {
      ...appState,
      exportBackground: true,
      exportWithDarkMode: false,
      viewBackgroundColor: "#ffffff",
    };
    const blob = await exportToBlob({
      elements: exportElements,
      appState: exportAppState,
      files,
      mimeType: "image/png",
      quality: 1,
      exportPadding: 16,
      ...(forLaser ? { getDimensions: getLaserExportDimensions } : {}),
    });
    const scene = {
      type: "excalidraw",
      version: 2,
      source: "excalidraw-laser-lab",
      elements,
      appState: {
        gridSize: appState.gridSize,
        name: appState.name,
        viewBackgroundColor: appState.viewBackgroundColor,
      },
      files,
    };
    setLastScene(scene);
    return { blob, scene };
  }

  async function generatePreview() {
    if (busy) {
      setStatus("正在导出并解算画板预览，请勿重复点击...");
      return;
    }
    if (workflowId) {
      const ok = await showConfirm("当前画板已生成过加工预览，确认要按当前画布重新生成吗？", {
        title: "重新生成确认",
        confirmText: "确认重新生成",
        cancelText: "保留当前预览",
      });
      if (!ok) return;
    }
    setBusy(true);
    setStatus("导出画板中...");
    try {
      const { blob, scene } = await exportScene({ forLaser: true });
      const imageDataUrl = await blobToDataUrl(blob);
      setStatus("上传并生成预览中...");
      const payload = {
        image_data_url: imageDataUrl,
        scene_json: scene,
        material: form.material,
        thickness_mm: Number(form.thickness_mm) || 3.0,
        width_mm: drawSizeValue(form.width_mm),
        height_mm: drawSizeValue(form.height_mm),
        mode: form.mode,
        output_format: form.output_format,
        task_type: taskTypeForMode(form.mode),
        ...drawConnectionPayload(form),
        lock_aspect_ratio: true,
      };
      if (form.mode === "raster") {
        payload.dither_algorithm = "threshold";
        payload.raster_scan_direction = "horizontal";
        payload.raster_output_strategy = "scanline";
        payload.raster_quality_strategy = "manual";
      }
      if (form.mode === "outline") {
        const simplifyRaw = String(form.vector_simplify_factor ?? "").trim();
        payload.vector_simplify_factor = simplifyRaw === "" ? 3 : Number(simplifyRaw);
      }
      const res = await postJson("/api/draw/preview", payload);
      const data = res.data || { success: false, result: `HTTP ${res.status}` };
      setResult(data);
      const nextWorkflowId = res.ok && data.success
        ? data?.result?.workflow_id || data?.result?.workflow?.workflow_id || ""
        : "";
      setWorkflowId(nextWorkflowId);
      setConfirmOpen(false);
      if (res.ok && data.success) {
        const fingerprint = data?.result?.draw_lab?.content_fingerprint || "";
        setRepeatSend((current) => applyPreviewFingerprintSuccess(current, fingerprint));
        // 未填尺寸且超出机器幅面时后端会等比缩小，并说明缩到多大（2026-11-06）
        const fitNote = data.result?.artifacts?.fit_note;
        setStatus([data.result?.speech || "预览已生成", fitNote].filter(Boolean).join("；"));
      } else {
        setStatus((typeof data.result === "string" && data.result) || "预览失败");
      }
    } catch (error) {
      setStatus(`失败：${error.message || error}`);
      setResult({ success: false, result: String(error.message || error) });
    } finally {
      setBusy(false);
    }
  }

  function openSendConfirmation() {
    if (!workflowId) return;
    if (!hostReady) {
      setStatus(drawTargetMissingText(form));
      return;
    }
    if (!summaryAllowsSend) {
      setStatus("当前预览不可发送：材料参数未验证或服务端未提供 confirm_send");
      return;
    }
    if (!canSubmitConfirmSend(repeatSend)) {
      setStatus("相同图案再次发送前，请勾选“已移动材料或原点”");
    }
    setConfirmOpen(true);
  }

  async function confirmSend(offlineSd = false) {
    if (!workflowId || !hostReady) {
      setStatus(workflowId ? drawTargetMissingText(form) : "请先生成预览");
      return;
    }
    if (!summaryAllowsSend) {
      setStatus("当前预览不可发送：材料参数未验证或服务端未提供 confirm_send");
      return;
    }
    if (!canSubmitConfirmSend(repeatSend)) {
      setStatus("相同图案再次发送前，请勾选“已移动材料或原点”");
      return;
    }
    setBusy(true);
    setStatus(offlineSd ? "提交脱机离线加工..." : "提交确认发送...");
    try {
      const res = await postJson("/api/workflow", {
        action: "confirm_send",
        workflow_id: workflowId,
        confirmed: true,
        ...drawConnectionPayload(form),
        offline_sd: Boolean(offlineSd) && !isSerial(form),
        run_in_background: true,
        wait_for_response: true,
      });
      const data = res.data || { success: false, result: `HTTP ${res.status}` };
      setResult(data);
      setConfirmOpen(false);
      setRepeatSend((current) =>
        applyConfirmSendResponse(current, res.ok ? data : { ...data, success: false }, safeBrowserStorage()),
      );
      setStatus(
        res.ok && data.success
          ? data.result?.speech || (offlineSd ? "已启动 SD 脱机加工" : "已交给发送门禁处理")
          : (typeof data.result === "string" && data.result) || "发送被拒绝",
      );
    } catch (error) {
      setStatus(`请求失败：${error.message || error}`);
    } finally {
      setBusy(false);
    }
  }

  async function runFraming() {
    if (busy) return;
    if (!hostReady) {
      setStatus(drawTargetMissingText(form));
      return;
    }
    const width = jobSummary?.actual_width_mm || 100;
    const height = jobSummary?.actual_height_mm || 100;
    // 走边框会实际移动机器（勾选弱光时还会低功率出光），单击不得直接执行（2026-11-05 用户决定）
    const lightOn = framingLight;
    const confirmed = await showConfirm(framingConfirmText(form, width, height, lightOn), {
      title: lightOn ? "确认走边框（弱光出光）" : "确认走边框（机器将移动）",
      tone: "warning",
      danger: lightOn,
      confirmText: lightOn ? "移动并弱光走边框" : "开始走边框",
      cancelText: "取消",
    });
    if (!confirmed) {
      setStatus("已取消走边框，机器未移动");
      return;
    }
    setBusy(true);
    setStatus(lightOn ? "正在走边框（弱光指示）…顶栏急停可随时停止" : "正在走边框（激光关闭，只移动激光头）…顶栏急停可随时停止");
    try {
      // 默认不出光；勾选“弱光指示”才以 S1 出极弱的光（2026-10-14）
      const res = await postJson("/api/device/framing", {
        ...drawConnectionPayload(form),
        min_x: 0,
        min_y: 0,
        max_x: width,
        max_y: height,
        feed_rate: 2000,
        power: lightOn ? 1 : 0,
      });
      setStatus(formatFramingStatus(res, width, height, lightOn));
    } catch (e) {
      setStatus(`走边框请求失败：${e.message || e}`);
    } finally {
      setBusy(false);
    }
  }

  async function testConnection() {
    if (!hostReady) {
      setConnectionResult({ success: false, result: drawTargetMissingText(form) });
      setStatus(drawTargetMissingText(form));
      return;
    }
    setConnectionBusy(true);
    setStatus("测试设备连接...");
    try {
      const res = await postJson("/api/connection/check", {
        ...drawConnectionPayload(form),
        network_transport: "telnet",
        network_timeout: 3,
        include_detail: false,
      });
      const data = res.data || { success: false, result: `HTTP ${res.status}` };
      setConnectionResult(data);
      setStatus(
        res.ok && data.success
          ? data.result?.summary || "连接检查完成"
          : (typeof data.result === "string" && data.result) || "连接检查失败",
      );
    } catch (error) {
      const message = error.message || String(error);
      setConnectionResult({ success: false, result: message });
      setStatus(`连接检查失败：${message}`);
    } finally {
      setConnectionBusy(false);
    }
  }

  async function loadSerialPorts() {
    try {
      const response = await apiFetch("/api/ui/serial-ports");
      const data = await response.json();
      const ports = Array.isArray(data?.result?.ports) ? data.result.ports : [];
      setSerialPorts(ports);
      if (ports.length && !ports.includes(formRef.current.port)) updateForm("port", ports[0]);
      setStatus(ports.length ? `发现串口：${ports.join(", ")}` : "未检测到可用串口");
    } catch (error) {
      setStatus(`读取串口列表失败：${error.message || error}`);
    }
  }

  // 顶栏常驻急停（2026-11-05）：只走 `/api/safe-action` 实时 0x18，绝不退化为 workflow cancel。
  // 顶栏已先停后端登记的任务；轮到本页说明任务已不在登记表（如刚全量发送完、GRBL 仍在执行缓冲）
  // 或任务通道挂起，此时按任务编号失败后改用本页设备目标新开连接写 0x18（`usePageEmergencyStop`）。
  usePageEmergencyStop(
    () => ({ jobId: workflowId, target: estopTargetArgs(form) }),
    (outcome) => {
      const message = outcome.kind === "sent"
        ? `${outcome.message || "急停 0x18 已发送"}。软件急停不代表物理停机，请到设备旁确认；必要时按实体急停。`
        : `${outcome.message || "急停发送失败"}。请立即使用实体急停，并检查设备连接。`;
      setStatus(message);
      void showAlert(message, { title: outcome.kind === "sent" ? "已发送急停" : "急停未确认", tone: outcome.kind === "sent" ? "warning" : "error" });
    },
  );

  async function workflowAction(action) {
    if (!workflowId) return;
    setBusy(true);
    setStatus(action === "status" ? "查询状态..." : "提交取消...");
    try {
      const res = await postJson("/api/workflow", { action, workflow_id: workflowId });
      const data = res.data || { success: false, result: `HTTP ${res.status}` };
      setResult(data);
      setStatus(
        res.ok && data.success
          ? data.result?.speech || "已更新"
          : (typeof data.result === "string" && data.result) || "操作失败",
      );
    } catch (error) {
      setStatus(`请求失败：${error.message || error}`);
    } finally {
      setBusy(false);
    }
  }

  async function downloadPng() {
    try {
      const { blob } = await exportScene();
      const saved = await exportFile(blob, "excalidraw-laser-lab.png", "PNG ");
      setStatus(savedMessage("PNG ", saved));
    } catch (error) {
      const message = `导出 PNG 失败：${error.message || error}`;
      setStatus(message);
      void showAlert(message, { title: "导出失败", tone: "error" });
    }
  }

  async function downloadScene() {
    try {
      const { scene } = lastScene ? { scene: lastScene } : await exportScene();
      const blob = new Blob([JSON.stringify(scene, null, 2)], { type: "application/json" });
      const saved = await exportFile(blob, "excalidraw-laser-lab.excalidraw.json", "场景 JSON ");
      setStatus(savedMessage("场景 JSON ", saved));
    } catch (error) {
      const message = `导出场景失败：${error.message || error}`;
      setStatus(message);
      void showAlert(message, { title: "导出失败", tone: "error" });
    }
  }

  // 预览结果里的 G-code / 场景下载：经鉴权请求取回文件，再统一保存（桌面端写入“下载”文件夹）
  async function downloadFromUrl(url, fallbackName, label) {
    try {
      const response = await apiFetch(url);
      if (!response.ok) throw new Error(`读取文件失败（HTTP ${response.status}）`);
      const blob = await response.blob();
      const saved = await exportFile(blob, fileNameFromUrl(url, fallbackName), label);
      setStatus(savedMessage(label, saved));
    } catch (error) {
      const message = `下载${label}失败：${error.message || error}`;
      setStatus(message);
      void showAlert(message, { title: "下载失败", tone: "error" });
    }
  }

  return (
    <main className="app-shell ob-page ob-draw-page">

      {/* 大画布占满整行，下方三张卡并排：材料与尺寸、设备、预览与发送（技术书 §9.5.5 B、r25；支持自由缩放/拖动/折叠） */}
      <section className="workspace ob-draw">
        <WorkbenchCanvas
          defaultLayout={DRAW_DEFAULT_LAYOUT}
          storageKey={DRAW_STORAGE_KEY}
          toolbarStart={
            <>
              <div className="topbar-actions">
                <button type="button" className="btn btn-ghost" onClick={() => onNavigate("/")}>
                  <Home size={14} aria-hidden="true" />
                  工作台
                </button>
                <button type="button" className="btn btn-ghost" onClick={downloadScene} disabled={busy || !excalidrawAPI}>
                  <FileJson size={14} aria-hidden="true" />
                  场景
                </button>
                <button type="button" className="btn btn-ghost" onClick={downloadPng} disabled={busy || !excalidrawAPI}>
                  <Download size={14} aria-hidden="true" />
                  PNG
                </button>
              </div>
              <div className="ob-canvasbar">
                <span className="draw-note" role="status">{status}</span>
              </div>
            </>
          }
        >
        <WbCard id="board" className="hud-card ob-card canvas-panel ob-board" label="画布">
          <FoldHeader id="draw-board" title="创作画布" k="CANVAS" />
          <div className="ob-board-stage" ref={boardStageRef}>
            <Excalidraw excalidrawAPI={setExcalidrawAPI} theme="dark" langCode={EXCALIDRAW_LANG_CODE} />
          </div>
        </WbCard>

        <WbCard id="draw-material" className="hud-card ob-card control-panel" label="材料与尺寸">
          <FoldHeader id="draw-material" title="材料与尺寸" k="MATERIAL" n={1} />
          <div className="material-library-label">
            <span>材料库</span>
            <div className="material-library-row">
              <select
                value={materialSelectValue}
                onChange={(event) => handleMaterialSelectChange(event.target.value)}
              >
                {materialLibrary.map((item) => (
                  <option key={item.name} value={item.name}>
                    {item.name}
                  </option>
                ))}
                <option value={CUSTOM_OPTION_VALUE}>{CUSTOM_OPTION_LABEL}</option>
              </select>
              <button
                type="button"
                className="ghost-button icon-button"
                title="刷新材料库"
                aria-label="刷新材料库"
                disabled={materialRefreshBusy}
                onClick={refreshMaterialLibrary}
              >
                <RefreshCw size={16} />
              </button>
            </div>
          </div>

          {materialMode === "custom" ? (
            <label>
              材料名称
              <input
                value={form.material}
                onChange={(event) => updateForm("material", event.target.value)}
              />
            </label>
          ) : null}

          {materialMode === "library" && findMaterialOption(materialLibrary, form.material) ? (
            <label>
              厚度 mm
              <select
                value={form.thickness_mm}
                onChange={(event) => handleThicknessSelectChange(event.target.value)}
              >
                {findMaterialOption(materialLibrary, form.material).thicknesses.map((value) => (
                  <option key={String(value)} value={String(value)}>
                    {value}
                  </option>
                ))}
              </select>
            </label>
          ) : (
            <label>
              厚度 mm
              <input
                type="number"
                min="0"
                step="0.1"
                value={form.thickness_mm}
                onChange={(event) => updateForm("thickness_mm", event.target.value)}
              />
            </label>
          )}

          {materialMode === "custom" && materialLibrary.length > 0 ? (
            <button type="button" className="ghost-button" onClick={handleReturnToLibrary}>
              返回材料库
            </button>
          ) : null}

          <div className="dimension-grid">
            <label>
              宽 mm
              <input
                type="number"
                min="0"
                max="100"
                step="0.1"
                value={form.width_mm}
                onChange={(event) => updateForm("width_mm", event.target.value)}
                placeholder="自动 (≤100)"
              />
            </label>
            <label>
              高 mm
              <input
                type="number"
                min="0"
                max="100"
                step="0.1"
                value={form.height_mm}
                onChange={(event) => updateForm("height_mm", event.target.value)}
                placeholder="自动 (≤100)"
              />
            </label>
          </div>

          <label>
            生成策略
            <select value={form.mode} onChange={(event) => updateForm("mode", event.target.value)}>
              <option value="raster">光栅雕刻 raster（逐行扫描填充）</option>
              <option value="outline">轮廓雕刻 outline（沿深色图形边缘描线）</option>
            </select>
          </label>

          {form.mode === "outline" ? (
            <label>
              轮廓简化 vector_simplify_factor
              <input
                type="number"
                min="0.25"
                max="8"
                step="0.05"
                value={form.vector_simplify_factor}
                onChange={(event) => updateForm("vector_simplify_factor", event.target.value)}
                placeholder="默认 3"
              />
            </label>
          ) : null}

        </WbCard>

        <WbCard id="draw-device" className="hud-card ob-card control-panel" label="设备">
          <FoldHeader id="draw-device" title="设备" k="DEVICE" n={2} />
          <label>
            连接方式
            <select value={form.connection_mode} onChange={(event) => updateForm("connection_mode", event.target.value)}>
              <option value="network">网络 (Telnet/TCP)</option>
              <option value="serial">串口 (USB/Serial)</option>
            </select>
          </label>

          {isSerial(form) ? (
            <>
              <label>
                串口
                <span className="draw-inline">
                  <select value={form.port} onChange={(event) => updateForm("port", event.target.value)}>
                    {form.port && !serialPorts.includes(form.port) ? <option value={form.port}>{form.port}</option> : null}
                    {serialPorts.length === 0 && !form.port ? <option value="">未检测到串口，请点刷新</option> : null}
                    {serialPorts.map((port) => (
                      <option key={port} value={port}>
                        {port}
                      </option>
                    ))}
                  </select>
                  <button type="button" className="ghost-button" onClick={loadSerialPorts} disabled={busy}>
                    <RefreshCw size={15} /> 刷新
                  </button>
                </span>
              </label>
              <label>
                波特率
                <select value={form.baudrate} onChange={(event) => updateForm("baudrate", event.target.value)}>
                  {["9600", "19200", "38400", "57600", "115200", "230400", "250000", "460800", "921600"].map((rate) => (
                    <option key={rate} value={rate}>
                      {rate}
                    </option>
                  ))}
                </select>
              </label>
            </>
          ) : (
            <>
              <label>
                设备 host/IP
                <input
                  value={form.network_host}
                  onChange={(event) => updateForm("network_host", event.target.value)}
                  placeholder="例如 laser.local"
                />
              </label>

              <label>
                Telnet 端口
                <input
                  type="number"
                  min="1"
                  step="1"
                  value={form.network_telnet_port}
                  onChange={(event) => updateForm("network_telnet_port", event.target.value)}
                />
              </label>
            </>
          )}

          <button
            type="button"
            className="secondary-button"
            onClick={testConnection}
            disabled={busy || connectionBusy}
          >
            <Wifi size={17} />
            {connectionBusy ? "测试中..." : "测试连接"}
          </button>
          <ConnectionStatus result={connectionResult} />

          <button type="button" className="secondary-button" onClick={saveConfiguration} disabled={busy}>
            <Save size={17} />
            保存配置
          </button>

        </WbCard>

        <WbCard id="draw-send" className="hud-card ob-card control-panel result-panel" label="预览与发送">
          <FoldHeader id="draw-send" title="预览与发送" k="PREVIEW · SEND" n={3} />
          <div className="action-stack">
            <button type="button" className="primary-button" onClick={generatePreview} disabled={busy || !excalidrawAPI}>
              <UploadCloud size={18} />
              生成预览
            </button>
            <button type="button" className="danger-button" onClick={openSendConfirmation} disabled={busy || !canConfirmSend}>
              <Send size={18} />
              确认发送
            </button>
            <button
              type="button"
              className="secondary-button draw-btn"
              onClick={runFraming}
              disabled={busy || !hostReady || !jobSummary?.actual_width_mm}
              title="控制激光头沿工件外接边框走一圈，用于对齐摆放材料；默认不出光"
            >
              <Eye size={18} />
              走边框 (Framing)
            </button>
            <label className="checkbox-row draw-label" title="勾选后走边框时以 S1（约 0.1% 功率）出极弱的光，便于在材料上看到边框">
              <input type="checkbox" checked={framingLight} onChange={(event) => setFramingLight(event.target.checked)} />
              走边框时弱光指示（默认不出光）
            </label>
            {canActOnWorkflow && !hostReady && <div className="inline-hint">{drawTargetMissingText(form)}后再发送</div>}
            {canActOnWorkflow && hostReady && !summaryAllowsSend && (
              <div className="inline-hint">当前不可发送：请检查精确厚度参数与服务端确认权限</div>
            )}
            <div className="split-actions">
              <button type="button" onClick={() => workflowAction("status")} disabled={busy || !canActOnWorkflow}>
                <Activity size={17} />
                状态
              </button>
              <button type="button" onClick={() => workflowAction("cancel")} disabled={busy || !canActOnWorkflow}>
                <Square size={17} />
                取消
              </button>
            </div>
          </div>

          <div className="workflow-chip">
            <RefreshCw size={16} />
            <span>{workflowId || "暂无 workflow"}</span>
          </div>

          <JobSummaryCard summary={jobSummary} />
          <FeedbackPanel workflowId={workflowId} origin={result?.result?.feedback_origin}
            disabled={busy || ["sending", "running", "preparing", "awaiting_completion"].includes(result?.result?.status)}
            onAdopt={(workflow) => {
              setWorkflowId(workflow.workflow_id);
              setResult({ success: true, result: workflow });
              setConfirmOpen(false);
              setRepeatSend((current) => applyPreviewFingerprintSuccess(current, workflow.draw_lab?.content_fingerprint || ""));
              setStatus("调参预览已生成，尚未发送");
            }} />
          <PreviewImage title="素材预览" url={web.material_preview_url || web.uploaded_image_url} />
          <PreviewImage title="路径预览" url={web.gcode_preview_url || web.image_preview_url} />
          <div className="link-list">
            {web.gcode_download_url && (
              <button
                type="button"
                className="secondary-button"
                onClick={() => downloadFromUrl(web.gcode_download_url, "laser-job.gcode", "G-code ")}
              >
                <Save size={16} />
                下载 G-code
              </button>
            )}
            {web.scene_download_url && (
              <button
                type="button"
                className="secondary-button"
                onClick={() => downloadFromUrl(web.scene_download_url, "excalidraw-laser-lab.excalidraw.json", "场景 JSON ")}
              >
                <FileJson size={16} />
                下载场景 JSON
              </button>
            )}
          </div>
          <details className="diagnostics">
            <summary className="diagnostics-title">
              <AlertTriangle size={16} />
              诊断输出
            </summary>
            <pre>{rawText || "等待预览结果..."}</pre>
          </details>
        </WbCard>
        </WorkbenchCanvas>
      </section>

      {confirmOpen && (
        <SendConfirmDialog
          summary={jobSummary}
          target={hostReady ? drawTargetLabel(form) : ""}
          sdAvailable={!isSerial(form)}
          busy={busy}
          sendAllowed={summaryAllowsSend}
          onCancel={() => setConfirmOpen(false)}
          onConfirm={confirmSend}
          requireMovedAck={requiresMovedMaterialAcknowledgement(repeatSend)}
          movedAcknowledged={Boolean(repeatSend.movedMaterialAcknowledged)}
          onMovedAckChange={(value) =>
            setRepeatSend((current) => setMovedMaterialAcknowledged(current, value))
          }
        />
      )}
    </main>
  );
}

function JobSummaryCard({ summary, compact = false }) {
  if (!summary) {
    return (
      <section className="job-summary-card muted-summary">
        <div className="summary-heading">
          <Settings2 size={17} />
          <h2>作业摘要</h2>
        </div>
        <p>生成预览后显示材料、尺寸、参数和预计时间。</p>
      </section>
    );
  }

  const displayWarnings = summaryDisplayWarnings(summary, formatWithUnit);
  const thicknessRows = thicknessDisplayRows(summary);
  const sizeText =
    summary.actual_width_mm && summary.actual_height_mm
      ? `${formatMm(summary.actual_width_mm)} x ${formatMm(summary.actual_height_mm)}`
      : "自动";

  return (
    <section className={`job-summary-card ${compact ? "compact" : ""}`}>
      <div className="summary-heading">
        <Settings2 size={17} />
        <h2>作业摘要</h2>
        <span className={summary.can_send ? "summary-badge ok" : "summary-badge warn"}>
          {sendBadgeLabel(summary)}
        </span>
      </div>
      <div className="summary-grid">
        <SummaryItem label="材料" value={summary.material || "未识别"} />
        {thicknessRows.map((row) => (
          <SummaryItem key={row.label} label={row.label} value={formatWithUnit(summary[row.valueKey], "mm")} />
        ))}
        <SummaryItem label="策略" value={modeLabel(summary.mode, summary.task_type)} />
        <SummaryItem label="尺寸" value={sizeText} icon={<Ruler size={15} />} />
        <SummaryItem label="预计时间" value={summary.estimated_display || "待估算"} icon={<Clock size={15} />} />
        <SummaryItem label="功率" value={summary.power ? `S${summary.power}` : "材料库默认"} />
        <SummaryItem label="速度" value={summary.speed ? `F${summary.speed}` : "材料库默认"} />
        <SummaryItem label="次数" value={summary.passes || "默认"} />
        <SummaryItem label="点距" value={formatWithUnit(summary.pixel_size_mm, "mm")} />
      </div>
      {summary.size_note && <p className="size-note">{summary.size_note}</p>}
      {displayWarnings.length > 0 && (
        <div className="warning-list">
          <ShieldAlert size={16} />
          <span>{displayWarnings.join("；")}</span>
        </div>
      )}
    </section>
  );
}

function SummaryItem({ label, value, icon = null }) {
  return (
    <div className="summary-item">
      <span>{label}</span>
      <strong>
        {icon}
        {value || "未知"}
      </strong>
    </div>
  );
}

function ConnectionStatus({ result }) {
  if (!result) return null;
  const payload = result.result && typeof result.result === "object" ? result.result : {};
  const connected = Boolean(payload.network_connected);
  const text = payload.summary || result.result || "连接检查失败";
  return (
    <div className={`connection-status ${result.success && connected ? "ok" : "warn"}`}>
      {result.success && connected ? <CheckCircle2 size={16} /> : <AlertTriangle size={16} />}
      <span>{text}</span>
    </div>
  );
}

function SendConfirmDialog({
  summary,
  target,
  sdAvailable = true,
  busy,
  sendAllowed = false,
  onCancel,
  onConfirm,
  requireMovedAck = false,
  movedAcknowledged = false,
  onMovedAckChange,
}) {
  const [offlineSdChoice, setOfflineSd] = React.useState(true);
  // SD 脱机只能经网口上传；串口只能逐行发送
  const offlineSd = sdAvailable && offlineSdChoice;
  const canConfirm = !requireMovedAck || movedAcknowledged;
  return (
    <div className="modal-backdrop" role="presentation">
      <section className="send-confirm-dialog" role="dialog" aria-modal="true" aria-labelledby="send-confirm-title">
        <div className="confirm-heading">
          <div>
            <Send size={18} />
            <h2 id="send-confirm-title">确认开始雕刻</h2>
          </div>
          <button type="button" className="icon-button" onClick={onCancel} aria-label="关闭确认面板">
            <X size={18} />
          </button>
        </div>
        <JobSummaryCard summary={summary} compact />
        <div className="confirm-device">
          <Wifi size={17} />
          <span>设备：{target}</span>
        </div>
        {sdAvailable ? (
          <label className="checkbox-row confirm-moved-ack draw-label">
            <input
              type="checkbox"
              checked={offlineSd}
              onChange={(e) => setOfflineSd(e.target.checked)}
            />
            脱机离线加工 (推荐，防电脑休眠/网络抖动；固件须启用 HashFS)
          </label>
        ) : null}
        {requireMovedAck ? (
          <label className="checkbox-row confirm-moved-ack">
            <input
              type="checkbox"
              checked={Boolean(movedAcknowledged)}
              onChange={(event) => onMovedAckChange?.(event.target.checked)}
            />
            已移动材料或原点（相同图案再次发送前必须勾选）
          </label>
        ) : null}
        <div className="confirm-actions">
          <button type="button" className="secondary-button" onClick={onCancel} disabled={busy}>
            取消
          </button>
          <button
            type="button"
            className="danger-button"
            onClick={() => onConfirm(offlineSd)}
            disabled={busy || !target || !sendAllowed || !canConfirm}
          >
            <Send size={18} />
            {busy ? "发送中..." : (offlineSd ? "离线脱机发送" : "在线流式发送")}
          </button>
        </div>
      </section>
    </div>
  );
}

function PreviewImage({ title, url }) {
  // 连接电脑时 <img> 直链不带配对 token，改为经鉴权取回 blob 再显示
  const remote = isRemoteMode();
  const [blobUrl, setBlobUrl] = useState("");
  useEffect(() => {
    if (!remote || !url) {
      setBlobUrl("");
      return undefined;
    }
    let cancelled = false;
    let created = "";
    fetchPreviewBlobUrl(url).then((value) => {
      created = value;
      if (cancelled) {
        if (value) URL.revokeObjectURL(value);
      } else {
        setBlobUrl(value);
      }
    });
    return () => {
      cancelled = true;
      if (created) URL.revokeObjectURL(created);
    };
  }, [remote, url]);
  const src = remote ? blobUrl : (url ? cacheBust(resolveApiUrl(url)) : "");
  return (
    <div className="preview-box">
      <div className="preview-title">{title}</div>
      {src ? <img src={src} alt={title} /> : <div className="preview-empty">暂无</div>}
    </div>
  );
}

function taskTypeForMode(mode) {
  if (mode === "raster") return "engrave_photo";
  if (mode === "outline") return "engrave_logo";
  return "";
}

function blobToDataUrl(blob) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(reader.error || new Error("读取 PNG 失败"));
    reader.readAsDataURL(blob);
  });
}

/** 从 `/preview?path=...` 下载地址取出文件名；取不到时用默认名 */
function fileNameFromUrl(url, fallback) {
  try {
    const parsed = new URL(url, "http://localhost");
    const path = parsed.searchParams.get("path") || parsed.pathname;
    const name = path.split(/[\\/]/).pop();
    return name && name.includes(".") ? name : fallback;
  } catch (_error) {
    return fallback;
  }
}

function cacheBust(url) {
  const separator = url.includes("?") ? "&" : "?";
  return `${url}${separator}t=${Date.now()}`;
}

function modeLabel(mode, taskType) {
  if (mode === "raster" || taskType === "engrave_photo") return "光栅雕刻";
  if (mode === "outline" || taskType === "engrave_logo") return "轮廓雕刻";
  if (taskType === "cut_contour") return "轮廓切割";
  return mode || taskType || "自动";
}

function formatMm(value) {
  const number = Number(value);
  if (!Number.isFinite(number)) return "未知";
  return `${Number(number.toFixed(2)).toString()} mm`;
}

function formatWithUnit(value, unit) {
  if (value === undefined || value === null || value === "") return "默认";
  const number = Number(value);
  if (!Number.isFinite(number)) return String(value);
  return `${Number(number.toFixed(3)).toString()} ${unit}`;
}
