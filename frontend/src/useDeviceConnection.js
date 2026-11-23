// 全局统一连接参数状态（2026-11-21 第三批重构 3B；2026-11-23 r58 全站共用同一份连接设置）：
// 初值读自统一连接存储，任一处（顶栏连接面板、工作台、创作画板、材料实验室）修改后只写单一存储源并通过自定义事件实时同步所有已挂载页面。
import { useEffect, useRef, useState } from "react";

import {
  DEVICE_CONNECTION_EVENT,
  readUnifiedConnection,
  writeUnifiedConnection,
} from "./deviceConnection.js";

/**
 * @param {{ numeric?: boolean }} [options] `numeric` 为真时波特率与 Telnet 端口按数字给出（工作台的表单和请求按数字处理），
 *   否则与存储一致按字符串给出（材料实验室 / 画板）。
 */
export function useDeviceConnection({ numeric = false } = {}) {
  const [initial] = useState(() => readUnifiedConnection());
  const [connectionMode, setConnectionMode] = useState(initial.connection_mode);
  const [serialPort, setSerialPort] = useState(initial.port);
  const [baudrate, setBaudrate] = useState(() =>
    numeric ? Number(initial.baudrate) || 115200 : initial.baudrate,
  );
  const [networkHost, setNetworkHost] = useState(initial.network_host);
  const [telnetPort, setTelnetPort] = useState(() =>
    numeric ? Number(initial.network_telnet_port) || 23 : initial.network_telnet_port,
  );

  const persistedRef = useRef(null);
  const syncingFromPeerRef = useRef(false);
  const instanceIdRef = useRef({});

  useEffect(() => {
    const applyExternal = (form) => {
      if (!form || typeof form !== "object") return;
      if (form._source === instanceIdRef.current) return;
      const nextMode = form.connection_mode === "serial" ? "serial" : "network";
      const nextPort = String(form.port ?? "");
      const nextBaudStr = String(form.baudrate || "115200");
      const nextHost = String(form.network_host ?? "");
      const nextTelnetStr = String(form.network_telnet_port || "23");
      const cur = persistedRef.current;
      if (
        cur
        && cur.connection_mode === nextMode
        && cur.port === nextPort
        && cur.baudrate === nextBaudStr
        && cur.network_host === nextHost
        && cur.network_telnet_port === nextTelnetStr
      ) {
        return;
      }
      syncingFromPeerRef.current = true;
      persistedRef.current = {
        connection_mode: nextMode,
        port: nextPort,
        baudrate: nextBaudStr,
        network_host: nextHost,
        network_telnet_port: nextTelnetStr,
      };
      setConnectionMode(nextMode);
      setSerialPort(nextPort);
      setBaudrate(numeric ? Number(nextBaudStr) || 115200 : nextBaudStr);
      setNetworkHost(nextHost);
      setTelnetPort(numeric ? Number(nextTelnetStr) || 23 : nextTelnetStr);
    };
    const onPeerChange = (event) => applyExternal(event?.detail);
    const onStorage = () => applyExternal(readUnifiedConnection());
    if (typeof window !== "undefined") {
      window.addEventListener(DEVICE_CONNECTION_EVENT, onPeerChange);
      window.addEventListener("storage", onStorage);
    }
    return () => {
      if (typeof window !== "undefined") {
        window.removeEventListener(DEVICE_CONNECTION_EVENT, onPeerChange);
        window.removeEventListener("storage", onStorage);
      }
    };
  }, [numeric]);

  useEffect(() => {
    const next = {
      connection_mode: connectionMode,
      port: serialPort,
      baudrate: String(baudrate),
      network_host: networkHost,
      network_telnet_port: String(telnetPort),
    };
    if (syncingFromPeerRef.current) {
      syncingFromPeerRef.current = false;
      persistedRef.current = next;
      return;
    }
    const previous = persistedRef.current;
    persistedRef.current = next;
    // 首次挂载不回写（值本来就读自存储）；之后只写变化的字段并向本窗口其他组件广播
    if (!previous) return;
    const changed = Object.fromEntries(Object.entries(next).filter(([field, value]) => previous[field] !== value));
    if (Object.keys(changed).length > 0) {
      writeUnifiedConnection(next);
      try {
        window.dispatchEvent(
          new CustomEvent(DEVICE_CONNECTION_EVENT, {
            detail: { ...next, _source: instanceIdRef.current },
          }),
        );
      } catch (_error) {
        // 非浏览器环境忽略广播
      }
    }
  }, [connectionMode, serialPort, baudrate, networkHost, telnetPort]);

  return {
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
  };
}
