import React, { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AlertTriangle, CheckCircle2, RefreshCw, Wifi, X, Zap } from "lucide-react";
import { DEFAULT_READ_TIMEOUT_MS, getJson, postJson } from "../api/client.js";
import { showPrompt } from "../appDialog.js";
import { controllerLabel, discoveryOutcome } from "../controllerDiscovery.js";
import {
  MACHINE_MOTION_EVENT,
  SERIAL_BAUDRATES,
  isHostSerialAvailable,
  probeControllerConnection,
  syncHardwareWorkspaceLimits,
} from "../deviceConnection.js";
import { machineMotionFromLimits, writeMachineMotion } from "../fillOptions.js";
import { defaultSerialPortHint } from "../platform.js";
import { useDeviceConnection } from "../useDeviceConnection.js";
import { isTopmostModalBackdrop } from "./AppDialog.jsx";

/**
 * 全局统一“设备连接”面板（2026-11-23 r58）：
 * - 点击顶栏硬件状态胶囊、工作台「修改连接」或创作画板「修改连接」打开同一面板；
 * - 打开面板、切换连接方式、修改 IP / 端口 / 串口均不自动发起连接或握手探测；
 * - 「扫描局域网」与「握手探测」仅在用户点击对应按钮时执行，不发送 G-code、不触发运动。
 */
export function DeviceConnectionModal(props) {
  if (!props.visible) return null;
  return <DeviceConnectionModalBody {...props} />;
}

function DeviceConnectionModalBody({
  onClose,
  connectionStatus,
  setConnectionStatus,
  onLog = null,
}) {
  const {
    connectionMode,
    setConnectionMode,
    serialPort,
    setSerialPort,
    baudrate,
    setBaudrate,
    networkHost,
    setNetworkHost,
    telnetPort,
    setTelnetPort,
  } = useDeviceConnection({ numeric: true });

  const [serialPorts, setSerialPorts] = useState([]);
  const [portsBusy, setPortsBusy] = useState(false);
  const [discovering, setDiscovering] = useState(false);
  const [discoveredChoices, setDiscoveredChoices] = useState([]);
  const [probing, setProbing] = useState(false);
  const [notice, setNotice] = useState(null);
  const [telnetPortText, setTelnetPortText] = useState(() => String(telnetPort || 23));

  const serialAvailable = isHostSerialAvailable();
  const effectiveMode = serialAvailable ? connectionMode : "network";

  useEffect(() => {
    if (!serialAvailable && connectionMode === "serial") {
      setConnectionMode("network");
    }
  }, [serialAvailable, connectionMode, setConnectionMode]);

  useEffect(() => {
    setTelnetPortText(String(telnetPort || 23));
  }, [telnetPort]);

  const targetKey =
    effectiveMode === "serial"
      ? `serial:${serialPort}:${baudrate}`
      : `network:${networkHost.trim()}:${telnetPort}`;
  const probeEpochRef = useRef(0);
  useEffect(() => {
    probeEpochRef.current += 1;
  }, [targetKey]);

  const backdropRef = useRef(null);
  const dialogRef = useRef(null);
  const modeSelectRef = useRef(null);
  const hostInputRef = useRef(null);
  const backdropPressRef = useRef(false);
  const mountedRef = useRef(true);
  const modalBusy = discovering || probing;

  useEffect(() => {
    mountedRef.current = true;
    const previousFocus = typeof document !== "undefined" ? document.activeElement : null;
    const focusTimer = typeof window !== "undefined"
      ? window.setTimeout(() => {
          if (connectionMode === "network" && hostInputRef.current) {
            hostInputRef.current.focus();
          } else {
            modeSelectRef.current?.focus();
          }
        }, 0)
      : null;
    return () => {
      mountedRef.current = false;
      if (focusTimer !== null) window.clearTimeout(focusTimer);
      if (previousFocus && typeof previousFocus.focus === "function" && document.contains(previousFocus)) {
        previousFocus.focus();
      }
    };
    // 仅在打开与卸载时捕获/还原焦点，切换连接方式不抢焦点
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const onKey = (e) => {
      if (e.defaultPrevented || e.isComposing || e.keyCode === 229) return;
      if (e.key === "Escape") {
        if (!isTopmostModalBackdrop(backdropRef.current)) return;
        e.preventDefault();
        if (modalBusy) return;
        onClose?.();
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
  }, [modalBusy, onClose]);

  const refreshSerialPorts = async () => {
    if (portsBusy) return;
    setPortsBusy(true);
    try {
      const res = await getJson("/api/ui/serial-ports", { timeoutMs: DEFAULT_READ_TIMEOUT_MS });
      if (!mountedRef.current) return;
      if (res.ok && res.data?.success) {
        const ports = Array.isArray(res.data.result?.ports) ? res.data.result.ports : [];
        setSerialPorts(ports);
        if (ports.length > 0 && !String(serialPort || "").trim()) {
          setSerialPort(ports[0]);
        }
        const text = ports.length > 0 ? `发现可用串口：${ports.join(", ")}` : "未检测到可用串口";
        setNotice({ tone: ports.length > 0 ? "ok" : "warn", text });
        onLog?.("SERIAL", text, ports.length > 0 ? "ok" : "warn");
      } else {
        setNotice({ tone: "warn", text: "读取串口列表失败" });
        onLog?.("SERIAL", "读取串口列表失败", "warn");
      }
    } catch (e) {
      if (mountedRef.current) {
        setNotice({ tone: "bad", text: `串口枚举异常：${e.message}` });
        onLog?.("SERIAL", `串口枚举异常：${e.message}`, "error");
      }
    } finally {
      if (mountedRef.current) setPortsBusy(false);
    }
  };

  const pickController = (controller) => {
    setNetworkHost(controller.ip);
    if (controller.telnet_port) setTelnetPort(Number(controller.telnet_port) || 23);
  };

  const handleDiscover = async () => {
    if (discovering || probing) return;
    setDiscovering(true);
    setDiscoveredChoices([]);
    setNotice({ tone: "info", text: "正在查找局域网里的雕刻机并逐台只读确认（约 1～5 秒）…" });
    try {
      const res = await postJson("/api/ui/discover-controllers", {});
      if (!mountedRef.current) return;
      if (!res.ok || !res.data?.success) {
        const msg = `扫描失败：${res.data?.result || res.error || "后端无响应"}`;
        setNotice({ tone: "bad", text: msg });
        onLog?.("DEVICE", msg, "error");
        return;
      }
      const outcome = discoveryOutcome(res.data.result);
      if (outcome.fill) pickController(outcome.fill);
      setDiscoveredChoices(outcome.choices);
      const tone = outcome.log.level === "ok" ? "ok" : outcome.log.level === "error" ? "bad" : "warn";
      setNotice({ tone, text: outcome.log.text });
      onLog?.("DEVICE", outcome.log.text, outcome.log.level);
    } catch (e) {
      if (!mountedRef.current) return;
      const msg = `扫描失败：${e.message}`;
      setNotice({ tone: "bad", text: msg });
      onLog?.("DEVICE", msg, "error");
    } finally {
      if (mountedRef.current) setDiscovering(false);
    }
  };

  const handleProbe = async () => {
    if (probing || discovering) return;
    if (connectionMode === "serial" && !String(serialPort || "").trim()) {
      setNotice({ tone: "warn", text: "请先选择或输入串口号后再握手探测" });
      return;
    }
    if (connectionMode === "network" && !String(networkHost || "").trim()) {
      setNotice({ tone: "warn", text: "请先填写控制器 IP 地址后再握手探测" });
      return;
    }
    const epoch = ++probeEpochRef.current;
    setProbing(true);
    const modeName = connectionMode === "serial" ? "串口" : "网络";
    setNotice({ tone: "info", text: `正在探测${modeName}控制器连接…` });
    onLog?.("DEVICE", `正在探测${modeName}控制器连接…`, "info");
    const effectiveTelnetPort = Number(telnetPortText) >= 1 && Number(telnetPortText) <= 65535
      ? Math.round(Number(telnetPortText))
      : (Number(telnetPort) || 23);
    if (connectionMode === "network" && effectiveTelnetPort !== telnetPort) {
      setTelnetPort(effectiveTelnetPort);
    }
    try {
      const outcome = await probeControllerConnection(postJson, {
        connection_mode: connectionMode,
        port: serialPort,
        baudrate,
        network_host: networkHost.trim(),
        network_telnet_port: effectiveTelnetPort,
      });
      if (!mountedRef.current || probeEpochRef.current !== epoch) return;
      setConnectionStatus?.(outcome.status);
      syncHardwareWorkspaceLimits(outcome.machineLimits);
      const motion = machineMotionFromLimits(outcome.machineLimits);
      if (motion) {
        writeMachineMotion(motion);
        try {
          window.dispatchEvent(new CustomEvent(MACHINE_MOTION_EVENT, { detail: motion }));
        } catch (_error) {
          // 忽略非浏览器环境广播失败
        }
      }
      setNotice({ tone: outcome.bannerTone, text: outcome.text });
      onLog?.("DEVICE", outcome.text, outcome.logLevel);
    } finally {
      if (mountedRef.current) setProbing(false);
    }
  };

  const statusBanner = notice
    ? notice
    : connectionStatus?.checkedAt
      ? {
          tone: connectionStatus.connected ? "ok" : "bad",
          text: connectionStatus.connected
            ? `上次探测通过：${connectionStatus.summary || "控制器已连接"}`
            : `上次探测未连通：${connectionStatus.summary || "未连接"}`,
        }
      : null;

  const content = (
    <div
      ref={backdropRef}
      className="mat-modal-backdrop device-conn-backdrop"
      onMouseDown={(e) => {
        backdropPressRef.current = e.button === 0 && e.target === e.currentTarget;
      }}
      onMouseUp={(e) => {
        if (e.target !== e.currentTarget) backdropPressRef.current = false;
      }}
      onClick={(e) => {
        if (backdropPressRef.current && e.target === e.currentTarget && !modalBusy) {
          onClose?.();
        }
        backdropPressRef.current = false;
      }}
    >
      <div
        ref={dialogRef}
        className="mat-modal device-conn-modal"
        id="device-connection-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="device-conn-modal-title"
      >
        <div className="mat-modal-head">
          <h3 id="device-conn-modal-title" className="settings-heading">
            <Zap size={18} className="settings-icon" aria-hidden="true" />
            <span>设备连接</span>
          </h3>
          <button
            type="button"
            className="mat-modal-close"
            id="device-conn-close"
            onClick={onClose}
            title="关闭"
            aria-label="关闭"
            disabled={modalBusy}
          >
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body device-conn-body">
          <p className="device-conn-lead">
            雕刻工作台、创作画板、材料实验室与 AI 预览共用这份连接设置；修改即时保存，不自动探测。
          </p>

          <div className="mat-field">
            <label htmlFor="conn-mode-select">连接方式</label>
            <select
              ref={modeSelectRef}
              id="conn-mode-select"
              value={effectiveMode}
              disabled={modalBusy || !serialAvailable}
              onChange={(e) => {
                setConnectionMode(e.target.value);
                setNotice(null);
              }}
              className="select-input wb-field"
            >
              <option value="network">网络 (Telnet / TCP)</option>
              {serialAvailable && <option value="serial">串口 (USB / Serial)</option>}
            </select>
          </div>

          {effectiveMode === "serial" ? (
            <>
              <div className="mat-field">
                <label htmlFor="conn-serial-port">串口</label>
                <div className="device-conn-inline">
                  <select
                    id="conn-serial-port"
                    value={serialPort}
                    disabled={modalBusy}
                    onChange={async (e) => {
                      const selectEl = e.target;
                      const chosen = selectEl.value;
                      if (chosen === "__custom__") {
                        selectEl.value = serialPort || "";
                        const serialHint = defaultSerialPortHint();
                        const custom = await showPrompt(serialHint.prompt, {
                          title: "手动输入串口号",
                          defaultValue: serialPort || serialHint.fallback,
                          placeholder: serialHint.fallback,
                        });
                        if (custom && custom.trim()) {
                          const p = custom.trim();
                          if (!serialPorts.includes(p)) {
                            setSerialPorts((prev) => [...prev, p]);
                          }
                          setSerialPort(p);
                          setNotice(null);
                        }
                      } else {
                        setSerialPort(chosen);
                        setNotice(null);
                      }
                    }}
                    className="select-input"
                  >
                    {serialPort && !serialPorts.includes(serialPort) ? (
                      <option value={serialPort}>{serialPort}</option>
                    ) : null}
                    {serialPorts.length === 0 && !serialPort ? (
                      <option value="" disabled>未扫描或无可用串口（点右侧刷新）</option>
                    ) : (
                      serialPorts.map((p) => (
                        <option key={p} value={p}>{p}</option>
                      ))
                    )}
                    <option value="__custom__">✎ 手动输入串口…</option>
                  </select>
                  <button
                    type="button"
                    id="conn-refresh-ports"
                    className="btn btn-ghost"
                    onClick={refreshSerialPorts}
                    disabled={portsBusy || modalBusy}
                    title="扫描系统可用串口"
                  >
                    <RefreshCw size={13} className={portsBusy ? "spin" : undefined} />
                    <span>刷新串口</span>
                  </button>
                </div>
              </div>

              <div className="mat-field">
                <label htmlFor="conn-baudrate">波特率</label>
                <select
                  id="conn-baudrate"
                  value={baudrate}
                  disabled={modalBusy}
                  onChange={(e) => {
                    setBaudrate(Number(e.target.value));
                    setNotice(null);
                  }}
                  className="select-input mono"
                >
                  {SERIAL_BAUDRATES.map((rate) => (
                    <option key={rate} value={rate}>
                      {rate === 115200 ? "115200（默认）" : rate}
                    </option>
                  ))}
                </select>
              </div>

              <p className="wb-hint" id="wb-serial-probe-hint">
                点“握手探测”会打开所选串口确认是不是雕刻机；部分 ESP32 / Arduino 板子打开串口时会自动重启，加工中不要探测别的串口。
              </p>
            </>
          ) : (
            <>
              <div className="device-conn-net-grid">
                <div className="mat-field">
                  <label htmlFor="conn-network-host">控制器 IP</label>
                  <input
                    ref={hostInputRef}
                    id="conn-network-host"
                    type="text"
                    placeholder="例 192.168.1.10"
                    value={networkHost}
                    disabled={modalBusy}
                    onChange={(e) => {
                      setNetworkHost(e.target.value);
                      setNotice(null);
                    }}
                    className="text-input mono"
                    spellCheck="false"
                    autoComplete="off"
                  />
                </div>
                <div className="mat-field">
                  <label htmlFor="conn-telnet-port">Telnet 端口</label>
                  <input
                    id="conn-telnet-port"
                    type="number"
                    min={1}
                    max={65535}
                    value={telnetPortText}
                    disabled={modalBusy}
                    onChange={(e) => {
                      const raw = e.target.value;
                      setTelnetPortText(raw);
                      const num = Number(raw);
                      if (raw.trim() !== "" && Number.isFinite(num) && num >= 1 && num <= 65535) {
                        setTelnetPort(Math.round(num));
                      }
                      setNotice(null);
                    }}
                    onBlur={() => {
                      const num = Number(telnetPortText);
                      if (telnetPortText.trim() === "" || !Number.isFinite(num) || num < 1 || num > 65535) {
                        setTelnetPortText(String(telnetPort || 23));
                      }
                    }}
                    className="text-input mono"
                  />
                </div>
              </div>

              {discoveredChoices.length > 1 ? (
                <div className="mat-field">
                  <label htmlFor="wb-discovered-controllers">选择发现的雕刻机</label>
                  <select
                    className="select-input"
                    id="wb-discovered-controllers"
                    disabled={modalBusy}
                    value={
                      discoveredChoices.find((c) => c.ip === networkHost && c.telnet_port === telnetPort)
                        ? `${networkHost}:${telnetPort}`
                        : ""
                    }
                    onChange={(e) => {
                      const picked = discoveredChoices.find((c) => `${c.ip}:${c.telnet_port}` === e.target.value);
                      if (picked) {
                        pickController(picked);
                        setNotice(null);
                      }
                    }}
                  >
                    <option value="" disabled>找到 {discoveredChoices.length} 台，请选择…</option>
                    {discoveredChoices.map((c) => (
                      <option key={`${c.ip}:${c.telnet_port}`} value={`${c.ip}:${c.telnet_port}`}>
                        {controllerLabel(c)}
                      </option>
                    ))}
                  </select>
                </div>
              ) : null}
            </>
          )}

          <div className={`wb-link-actions${effectiveMode === "network" ? " is-dual" : ""}`}>
            {effectiveMode === "network" ? (
              <button
                type="button"
                id="conn-discover-btn"
                className="btn btn-ghost"
                onClick={handleDiscover}
                disabled={modalBusy}
                title="查找局域网里的设备（mDNS + 本网段 23 端口），逐台发送只读状态查询，只列出按 GRBL / FluidNC 回应的雕刻机"
              >
                <RefreshCw size={13} className={`wb-icon${discovering ? " spin" : ""}`} />
                <span>{discovering ? "扫描中…" : "扫描局域网"}</span>
              </button>
            ) : null}
            <button
              type="button"
              id="conn-probe-btn"
              className="btn btn-hud-primary wb-btn"
              onClick={handleProbe}
              disabled={modalBusy}
            >
              <Wifi size={14} className="wb-icon" />
              <span>{probing ? "探测中…" : "握手探测"}</span>
            </button>
          </div>

          {statusBanner ? (
            <div
              id="conn-probe-status"
              role="status"
              className={`device-conn-status is-${statusBanner.tone}`}
            >
              {statusBanner.tone === "ok" ? (
                <CheckCircle2 size={15} aria-hidden="true" />
              ) : statusBanner.tone === "bad" || statusBanner.tone === "warn" ? (
                <AlertTriangle size={15} aria-hidden="true" />
              ) : null}
              <span>{statusBanner.text}</span>
            </div>
          ) : null}
        </div>

        <div className="mat-modal-foot">
          <button
            type="button"
            className="btn btn-ghost"
            onClick={onClose}
            disabled={modalBusy}
          >
            完成
          </button>
        </div>
      </div>
    </div>
  );

  return typeof document !== "undefined" && document.body
    ? createPortal(content, document.body)
    : content;
}
