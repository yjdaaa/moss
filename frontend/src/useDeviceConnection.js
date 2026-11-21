// 工作台连接参数状态（雕刻工作台与材料实验室共用，2026-11-21 第三批重构 3B）：
// 初值读自工作台存储键，改动后只把变化的字段写回，切页面后另一页读到的就是最新值。
import { useEffect, useRef, useState } from "react";

import { readWorkbenchConnection, writeWorkbenchConnection } from "./deviceConnection.js";

/**
 * @param {{ numeric?: boolean }} [options] `numeric` 为真时波特率与 Telnet 端口按数字给出（工作台的表单和请求按数字处理），
 *   否则与存储一致按字符串给出（材料实验室）。
 */
export function useDeviceConnection({ numeric = false } = {}) {
  const [initial] = useState(() => readWorkbenchConnection());
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
  useEffect(() => {
    const next = {
      connection_mode: connectionMode,
      port: serialPort,
      baudrate: String(baudrate),
      network_host: networkHost,
      network_telnet_port: String(telnetPort),
    };
    const previous = persistedRef.current;
    persistedRef.current = next;
    // 首次挂载不回写（值本来就读自存储）；之后只写变化的字段，避免覆盖其他页面刚改的键
    if (!previous) return;
    const changed = Object.fromEntries(Object.entries(next).filter(([field, value]) => previous[field] !== value));
    if (Object.keys(changed).length > 0) writeWorkbenchConnection(changed);
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
