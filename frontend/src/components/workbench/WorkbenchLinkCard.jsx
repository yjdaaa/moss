import React from "react";
import { RefreshCw } from "lucide-react";
import { showPrompt } from "../../appDialog.js";
import { controllerLabel } from "../../controllerDiscovery.js";
import { defaultSerialPortHint } from "../../platform.js";
import { FoldHeader } from "../FoldHeader.jsx";
import { WbCard } from "../WorkbenchCanvas.jsx";

/** 雕刻工作台 ① 通信链路卡：连接方式、串口 / 网络目标、局域网扫描与握手探测。原样搬自 pages/HomePage.jsx（2026-11-16 按职责拆分） */
export function WorkbenchLinkCard({
  connectionMode,
  setConnectionMode,
  serialPort,
  setSerialPort,
  serialPorts,
  setSerialPorts,
  baudrate,
  setBaudrate,
  loadSerialPorts,
  networkHost,
  setNetworkHost,
  telnetPort,
  setTelnetPort,
  discoveredChoices,
  pickController,
  discovering,
  discoverControllers,
  handleTestConnection,
}) {
  return (
    <WbCard id="link" className="hud-card ob-card" label="通信链路">
      <FoldHeader id="wb-link" title="通信链路" k="LINK" n={1} />
      {/* Connection Bar */}
      <div
        className="connection-bar wb-row"
      >
        <div className="wb-row-2">
          <span className="wb-text-2">连接方式</span>
          <select
            value={connectionMode}
            onChange={(e) => setConnectionMode(e.target.value)}
            className="select-input wb-field"
          >
            <option value="network">网络 (Telnet/TCP)</option>
            <option value="serial">串口 (USB/Serial)</option>
          </select>
        </div>

        {connectionMode === "serial" ? (
          <div className="wb-row-2">
            <select
              value={serialPort}
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
                  }
                } else {
                  setSerialPort(chosen);
                }
              }}
              className="select-input"
            >
              {serialPort && !serialPorts.includes(serialPort) ? (
                <option value={serialPort}>{serialPort}</option>
              ) : null}
              {serialPorts.length === 0 && !serialPort ? (
                <option value="" disabled>未检测到可用串口 (请点击右侧刷新)</option>
              ) : (
                serialPorts.map((p) => (
                  <option key={p} value={p}>{p}</option>
                ))
              )}
              <option value="__custom__">✎ 手动输入串口…</option>
            </select>
            <select
              value={baudrate}
              onChange={(e) => setBaudrate(Number(e.target.value))}
              className="select-input"
            >
              <option value={9600}>9600 波特率</option>
              <option value={19200}>19200 波特率</option>
              <option value={38400}>38400 波特率</option>
              <option value={57600}>57600 波特率</option>
              <option value={115200}>115200 波特率 (默认)</option>
              <option value={230400}>230400 波特率</option>
              <option value={250000}>250000 波特率</option>
              <option value={460800}>460800 波特率</option>
              <option value={921600}>921600 波特率</option>
            </select>
            <button className="btn btn-ghost" onClick={() => loadSerialPorts(true)} title="如同串口助手立即重新扫描系统串口">
              <RefreshCw size={13} />
            </button>
          </div>
        ) : (
          <div className="wb-row-2">
            <input
              type="text"
              placeholder="控制器 IP (例 192.168.1.10)"
              value={networkHost}
              onChange={(e) => setNetworkHost(e.target.value)}
              className="text-input wb-field-2"
            />
            <span className="wb-text-2">端口</span>
            <input
              type="number"
              value={telnetPort}
              onChange={(e) => setTelnetPort(Number(e.target.value))}
              className="text-input wb-field-3"
            />
          </div>
        )}
        {connectionMode === "network" && discoveredChoices.length > 1 && (
          <div className="wb-row-2">
            <span className="wb-text-2">选择雕刻机:</span>
            <select
              className="select-input wb-field"
              id="wb-discovered-controllers"
              value={discoveredChoices.find((c) => c.ip === networkHost && c.telnet_port === telnetPort) ? `${networkHost}:${telnetPort}` : ""}
              onChange={(e) => {
                const picked = discoveredChoices.find((c) => `${c.ip}:${c.telnet_port}` === e.target.value);
                if (picked) pickController(picked);
              }}
            >
              <option value="" disabled>找到 {discoveredChoices.length} 台，请选择…</option>
              {discoveredChoices.map((c) => (
                <option key={`${c.ip}:${c.telnet_port}`} value={`${c.ip}:${c.telnet_port}`}>{controllerLabel(c)}</option>
              ))}
            </select>
          </div>
        )}
        {connectionMode === "serial" && (
          <p className="wb-hint" id="wb-serial-probe-hint">
            点“握手探测”会打开所选串口确认是不是雕刻机；部分 ESP32 / Arduino 板子打开串口时会自动重启，加工中不要探测别的串口。
          </p>
        )}

        <div className={`wb-link-actions${connectionMode === "network" ? " is-dual" : ""}`}>
          {connectionMode === "network" && (
            <button
              className="btn btn-ghost"
              onClick={discoverControllers}
              disabled={discovering}
              title="查找局域网里的设备（mDNS 广播 + 本网段 23 端口），逐台发送只读的状态查询，只列出按 GRBL / FluidNC 回应的雕刻机"
            >
              <RefreshCw size={13} className="wb-icon" />
              <span>{discovering ? "扫描中…" : "扫描局域网"}</span>
            </button>
          )}
          <button className="btn btn-ghost wb-btn" onClick={handleTestConnection}>
            <RefreshCw size={13} className="wb-icon" />
            <span>握手探测</span>
          </button>
        </div>
      </div>
    </WbCard>
  );
}
