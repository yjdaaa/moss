// 全局统一连接参数状态（2026-11-21 第三批重构 3B；2026-11-23 r58 全站共用同一份连接设置）：
// 初值读自统一连接存储，任一处（顶栏连接面板、工作台、创作画板、材料实验室）修改后只写单一存储源并通过自定义事件实时同步所有已挂载页面。
import { useCallback, useEffect, useRef, useState } from "react";

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
  const [connectionMode, setConnectionModeState] = useState(initial.connection_mode);
  const [serialPort, setSerialPortState] = useState(initial.port);
  const [baudrate, setBaudrateState] = useState(() =>
    numeric ? Number(initial.baudrate) || 115200 : initial.baudrate,
  );
  const [networkHost, setNetworkHostState] = useState(initial.network_host);
  const [telnetPort, setTelnetPortState] = useState(() =>
    numeric ? Number(initial.network_telnet_port) || 23 : initial.network_telnet_port,
  );

  // 同步追踪已落盘表单与当前状态，在 setter 内立即写入并广播事件，
  // 防止弹窗改完 IP 紧接着按 Esc 关闭时，异步 useEffect 尚未运行就被卸载导致改动丢失。
  const persistedRef = useRef({
    connection_mode: initial.connection_mode,
    port: initial.port,
    baudrate: String(initial.baudrate || "115200"),
    network_host: initial.network_host,
    network_telnet_port: String(initial.network_telnet_port || "23"),
  });
  const stateRef = useRef({
    connection_mode: connectionMode,
    port: serialPort,
    baudrate,
    network_host: networkHost,
    network_telnet_port: telnetPort,
  });
  stateRef.current = {
    connection_mode: connectionMode,
    port: serialPort,
    baudrate,
    network_host: networkHost,
    network_telnet_port: telnetPort,
  };
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
        cur.connection_mode === nextMode
        && cur.port === nextPort
        && cur.baudrate === nextBaudStr
        && cur.network_host === nextHost
        && cur.network_telnet_port === nextTelnetStr
      ) {
        return;
      }
      const nextBaud = numeric ? Number(nextBaudStr) || 115200 : nextBaudStr;
      const nextTelnet = numeric ? Number(nextTelnetStr) || 23 : nextTelnetStr;
      persistedRef.current = {
        connection_mode: nextMode,
        port: nextPort,
        baudrate: nextBaudStr,
        network_host: nextHost,
        network_telnet_port: nextTelnetStr,
      };
      stateRef.current = {
        connection_mode: nextMode,
        port: nextPort,
        baudrate: nextBaud,
        network_host: nextHost,
        network_telnet_port: nextTelnet,
      };
      setConnectionModeState(nextMode);
      setSerialPortState(nextPort);
      setBaudrateState(nextBaud);
      setNetworkHostState(nextHost);
      setTelnetPortState(nextTelnet);
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

  const updateField = useCallback((field, setState, valueOrUpdater) => {
    const prevState = stateRef.current[field];
    const resolved = typeof valueOrUpdater === "function" ? valueOrUpdater(prevState) : valueOrUpdater;
    stateRef.current = { ...stateRef.current, [field]: resolved };
    setState(resolved);

    const prevPersisted = persistedRef.current;
    const nextStr = String(resolved);
    if (prevPersisted[field] === nextStr) return;
    const nextPersisted = { ...prevPersisted, [field]: nextStr };
    persistedRef.current = nextPersisted;
    writeUnifiedConnection(nextPersisted);
    try {
      if (typeof window !== "undefined" && typeof window.dispatchEvent === "function") {
        window.dispatchEvent(
          new CustomEvent(DEVICE_CONNECTION_EVENT, {
            detail: { ...nextPersisted, _source: instanceIdRef.current },
          }),
        );
      }
    } catch (_error) {
      // 非浏览器环境忽略广播
    }
  }, []);

  const setConnectionMode = useCallback((v) => updateField("connection_mode", setConnectionModeState, v), [updateField]);
  const setSerialPort = useCallback((v) => updateField("port", setSerialPortState, v), [updateField]);
  const setBaudrate = useCallback((v) => updateField("baudrate", setBaudrateState, v), [updateField]);
  const setNetworkHost = useCallback((v) => updateField("network_host", setNetworkHostState, v), [updateField]);
  const setTelnetPort = useCallback((v) => updateField("network_telnet_port", setTelnetPortState, v), [updateField]);

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
