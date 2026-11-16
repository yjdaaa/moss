import React from "react";
import { AlertTriangle, CheckCircle2, RefreshCw, Save, Wifi } from "lucide-react";
import { showPrompt } from "../../appDialog.js";
import { controllerLabel } from "../../controllerDiscovery.js";
import { isDrawConnectionOk, isSerial } from "../../drawConnection.js";
import { defaultSerialPortHint } from "../../platform.js";
import { FoldHeader } from "../FoldHeader.jsx";
import { WbCard } from "../WorkbenchCanvas.jsx";

/** 创意画板 ② 设备卡：连接方式、串口 / 网络目标、局域网扫描、测试连接与保存配置。原样搬自 pages/DrawPage.jsx（2026-11-16 按职责拆分） */
export function DrawDeviceCard({
  form,
  updateForm,
  busy,
  serialPorts,
  setSerialPorts,
  loadSerialPorts,
  discoveredChoices,
  pickDiscoveredController,
  discovering,
  discoverControllers,
  connectionBusy,
  testConnection,
  connectionResult,
  saveConfiguration,
}) {
  return (
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
              <select
                value={form.port}
                onChange={async (event) => {
                  const selectEl = event.target;
                  const chosen = selectEl.value;
                  if (chosen === "__custom__") {
                    selectEl.value = form.port || "";
                    const hint = defaultSerialPortHint();
                    const custom = await showPrompt(hint.prompt, {
                      title: "手动输入串口号",
                      defaultValue: form.port || hint.fallback,
                      placeholder: hint.fallback,
                    });
                    if (custom && custom.trim()) {
                      const p = custom.trim();
                      if (!serialPorts.includes(p)) setSerialPorts((prev) => [...prev, p]);
                      updateForm("port", p);
                    }
                  } else {
                    updateForm("port", chosen);
                  }
                }}
              >
                {form.port && !serialPorts.includes(form.port) ? <option value={form.port}>{form.port}</option> : null}
                {serialPorts.length === 0 && !form.port ? <option value="">未检测到串口，请点刷新</option> : null}
                {serialPorts.map((port) => (
                  <option key={port} value={port}>
                    {port}
                  </option>
                ))}
                <option value="__custom__">✎ 手动输入串口…</option>
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
            控制器 IP / 地址
            <input
              value={form.network_host}
              onChange={(event) => updateForm("network_host", event.target.value)}
              placeholder="例如 192.168.1.10 或 laser.local"
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

          {discoveredChoices.length > 1 ? (
            <label>
              选择发现的雕刻机
              <select
                id="draw-discovered-controllers"
                value={
                  discoveredChoices.find((c) => c.ip === form.network_host && String(c.telnet_port) === String(form.network_telnet_port))
                    ? `${form.network_host}:${form.network_telnet_port}`
                    : ""
                }
                onChange={(event) => {
                  const picked = discoveredChoices.find((c) => `${c.ip}:${c.telnet_port}` === event.target.value);
                  if (picked) pickDiscoveredController(picked);
                }}
              >
                <option value="" disabled>找到 {discoveredChoices.length} 台，请选择…</option>
                {discoveredChoices.map((c) => (
                  <option key={`${c.ip}:${c.telnet_port}`} value={`${c.ip}:${c.telnet_port}`}>
                    {controllerLabel(c)}
                  </option>
                ))}
              </select>
            </label>
          ) : null}

          <button
            type="button"
            id="draw-discover-btn"
            className="secondary-button"
            onClick={discoverControllers}
            disabled={busy || discovering || connectionBusy}
            title="查找局域网里的雕刻机并逐台只读确认"
          >
            <RefreshCw size={16} />
            {discovering ? "扫描局域网中..." : "扫描局域网"}
          </button>
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
      <ConnectionStatus form={form} result={connectionResult} />

      <button type="button" className="secondary-button" onClick={saveConfiguration} disabled={busy} title="各项修改会自动记住，也可点此立即核验本地存储">
        <Save size={17} />
        保存配置
      </button>

    </WbCard>
  );
}

function ConnectionStatus({ form, result }) {
  if (!result) return null;
  const payload = result.result && typeof result.result === "object" ? result.result : {};
  const connected = isDrawConnectionOk(form, result);
  const text = payload.summary || result.result || "连接检查失败";
  return (
    <div className={`connection-status ${connected ? "ok" : "warn"}`}>
      {connected ? <CheckCircle2 size={16} /> : <AlertTriangle size={16} />}
      <span>{text}</span>
    </div>
  );
}
