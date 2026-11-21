import React from "react";
import { RefreshCw } from "lucide-react";
import { showPrompt } from "../../appDialog.js";
import { defaultSerialPortHint } from "../../platform.js";

/** 材料实验室 ③ 校准矩阵 · 设备连接行（网络与 USB 串口，与工作台共享连接配置）。原样搬自 pages/MaterialLabPage.jsx（2026-11-16 按职责拆分） */
export function LabConnectionBar({
  connectionMode,
  setConnectionMode,
  serialPort,
  setSerialPort,
  serialPorts,
  setSerialPorts,
  baudrate,
  setBaudrate,
  loadSerialPorts,
  matrixHost,
  setMatrixHost,
  telnetPort,
  setTelnetPort,
}) {
  return (
    <div className="lab-conn-bar">
      <select
        value={connectionMode}
        onChange={(e) => {
          const next = e.target.value;
          setConnectionMode(next);
        }}
        className="select-input lab-conn-mode"
        aria-label="设备连接方式"
      >
        <option value="network">网络 IP</option>
        <option value="serial">USB 串口</option>
      </select>

      {connectionMode === "serial" ? (
        <div className="lab-conn-target">
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
            className="select-input lab-field-5"
            aria-label="选择串口"
          >
            {serialPort && !serialPorts.includes(serialPort) && (
              <option value={serialPort}>{serialPort}</option>
            )}
            {serialPorts.length === 0 && !serialPort && (
              <option value="">未检测到串口</option>
            )}
            {serialPorts.map((p) => (
              <option key={p} value={p}>{p}</option>
            ))}
            <option value="__custom__">✎ 手动输入串口…</option>
          </select>
          <select
            value={baudrate}
            onChange={(e) => {
              setBaudrate(e.target.value);
            }}
            title="串口波特率"
            className="select-input lab-baud-select"
            aria-label="串口波特率"
          >
            {["9600", "57600", "115200", "230400", "250000", "460800", "921600"].map((b) => (
              <option key={b} value={b}>{b}</option>
            ))}
          </select>
          <button
            type="button"
            className="btn btn-ghost lab-icon-btn"
            onClick={loadSerialPorts}
            title="刷新可用串口"
          >
            <RefreshCw size={12} />
          </button>
        </div>
      ) : (
        <div className="lab-conn-target">
          <input
            type="text"
            placeholder="设备 host / IP"
            value={matrixHost}
            onChange={(e) => {
              const val = e.target.value;
              setMatrixHost(val);
            }}
            className="text-input lab-field-5"
          />
          <input
            type="number"
            min={1}
            max={65535}
            value={telnetPort}
            onChange={(e) => {
              setTelnetPort(e.target.value);
            }}
            title="Telnet 端口（默认 23）"
            className="text-input lab-port-input"
          />
        </div>
      )}
    </div>
  );
}
