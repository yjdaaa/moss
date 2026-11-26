import test from "node:test";
import assert from "node:assert/strict";
import {
  DRAW_CONNECTION_KEYS,
  SERIAL_BAUDRATES,
  WORKBENCH_CONNECTION_KEYS,
  clearBufferedDeviceLogs,
  connectionSummaryView,
  emitDeviceLog,
  isHostSerialAvailable,
  probeControllerConnection,
  readBufferedDeviceLogs,
  readConnectionForm,
  readRawConnection,
  readUnifiedConnection,
  readWorkbenchConnection,
  writeUnifiedConnection,
  writeWorkbenchConnection,
} from "../src/deviceConnection.js";

function memoryStorage(initial = {}) {
  const data = new Map(Object.entries(initial));
  return {
    data,
    getItem: (key) => (data.has(key) ? data.get(key) : null),
    setItem: (key, value) => data.set(key, String(value)),
    removeItem: (key) => data.delete(key),
  };
}

const throwingStorage = {
  getItem() {
    throw new Error("SecurityError");
  },
  setItem() {
    throw new Error("SecurityError");
  },
};

test("工作台连接：没存过时给默认值，模式只认 serial / network", () => {
  assert.deepEqual(readWorkbenchConnection(memoryStorage()), {
    connection_mode: "network",
    network_host: "",
    network_telnet_port: "23",
    port: "",
    baudrate: "115200",
  });
  const storage = memoryStorage({
    "laser-forge-conn-mode": "serial",
    "laser-forge-serial-port": "COM3",
    "laser-forge-baud": "250000",
  });
  assert.equal(readWorkbenchConnection(storage).connection_mode, "serial");
  assert.equal(readWorkbenchConnection(storage).baudrate, "250000");
  assert.equal(readWorkbenchConnection(memoryStorage({ "laser-forge-conn-mode": "bogus" })).connection_mode, "network");
});

test("读写存储抛错时退回默认值、写入静默跳过（急停路径不能因此中断）", () => {
  assert.equal(readWorkbenchConnection(throwingStorage).connection_mode, "network");
  assert.doesNotThrow(() => writeWorkbenchConnection({ network_host: "<device-ip>" }, throwingStorage));
  assert.equal(readWorkbenchConnection(null).baudrate, "115200");
});

test("只写给出的字段，未知字段忽略，值转字符串", () => {
  const storage = memoryStorage({ "laser-forge-host": "keep" });
  writeWorkbenchConnection({ baudrate: 115200, network_telnet_port: 23, unknown: "x" }, storage);
  assert.equal(storage.data.get("laser-forge-baud"), "115200");
  assert.equal(storage.data.get("laser-forge-telnet-port"), "23");
  assert.equal(storage.data.get("laser-forge-host"), "keep");
  assert.equal(storage.data.size, 3);
});

test("原样读取区分没填与默认值；两组键互不串用", () => {
  const storage = memoryStorage({ "laserLab.network_host": "lab-host" });
  assert.deepEqual(readRawConnection(DRAW_CONNECTION_KEYS, storage), {
    connection_mode: null,
    network_host: "lab-host",
    network_telnet_port: null,
    port: null,
    baudrate: null,
  });
  assert.equal(readConnectionForm(WORKBENCH_CONNECTION_KEYS, storage).network_host, "");
  assert.equal(readConnectionForm(DRAW_CONNECTION_KEYS, storage).network_host, "lab-host");
});

test("统一连接存储与紧凑摘要：兼容旧画板回退，写入后只保留单一存储源并正确格式化状态", () => {
  const migrated = memoryStorage({
    "laser-forge-conn-mode": "network",
    "laser-forge-host": "",
    "laserLab.connection_mode": "network",
    "laserLab.network_host": "<device-ip>",
    "laserLab.network_telnet_port": "2323",
  });
  assert.deepEqual(readUnifiedConnection(migrated), {
    connection_mode: "network",
    network_host: "<device-ip>",
    network_telnet_port: "2323",
    port: "",
    baudrate: "115200",
  });

  writeUnifiedConnection({ connection_mode: "serial", port: "COM6", baudrate: 250000 }, migrated);
  assert.equal(migrated.data.has("laserLab.network_host"), false, "统一写入后应清掉旧画板连接副本");
  assert.equal(readUnifiedConnection(migrated).connection_mode, "serial");
  assert.equal(readUnifiedConnection(migrated).port, "COM6");
  assert.doesNotThrow(() => writeUnifiedConnection({ network_host: "<device-ip>" }, throwingStorage));

  assert.deepEqual(connectionSummaryView({ connection_mode: "network", network_host: "" }, null), {
    modeLabel: "网络",
    targetReady: false,
    targetText: "未填 IP",
    statusText: "未探测",
    statusTone: "off",
  });
  assert.deepEqual(
    connectionSummaryView(
      { connection_mode: "network", network_host: "<device-ip>", network_telnet_port: "23" },
      { connected: true, checkedAt: 1000, summary: "Idle" },
    ),
    {
      modeLabel: "网络",
      targetReady: true,
      targetText: "<device-ip>:23",
      statusText: "握手通过",
      statusTone: "ok",
    },
  );
  assert.deepEqual(
    connectionSummaryView(
      { connection_mode: "serial", port: "COM3", baudrate: "115200" },
      { connected: false, checkedAt: 1000, summary: "打开串口失败" },
    ),
    {
      modeLabel: "串口",
      targetReady: true,
      targetText: "COM3 · 115200",
      statusText: "打开串口失败",
      statusTone: "bad",
    },
  );
});

test("旧数据迁移：旧工作台只有残留默认串口、画板 laserLab.* 存了 network IP 时，统一读出 network + 该 IP 且写回后不丢", () => {
  const legacy = memoryStorage({
    // 旧工作台启动枚举时自动落下的默认串口，connection_mode 与 host 均未设置
    "laser-forge-serial-port": "/dev/cu.Bluetooth-Incoming-Port",
    // 用户此前在创作画板配好的真实网络控制器
    "laserLab.connection_mode": "network",
    "laserLab.network_host": "<device-ip>",
    "laserLab.network_telnet_port": "2323",
  });

  const unified = readUnifiedConnection(legacy);
  assert.equal(unified.connection_mode, "network", "残留串口不得在无模式下遮蔽画板已配好的网络模式");
  assert.equal(unified.network_host, "<device-ip>", "必须读出画板保存的控制器 IP");
  assert.equal(unified.network_telnet_port, "2323");

  // 部分字段写回（如仅改端口或波特率）后，旧画板键被清理，但迁移出的 network_host 必须已完整落入新存储源且不丢失
  writeUnifiedConnection({ network_telnet_port: "23" }, legacy);
  assert.equal(legacy.data.has("laserLab.network_host"), false, "写回后清理旧 laserLab 副本");
  const afterPartialWrite = readUnifiedConnection(legacy);
  assert.equal(afterPartialWrite.connection_mode, "network");
  assert.equal(afterPartialWrite.network_host, "<device-ip>", "部分写回后原画板控制器 IP 不得丢失");
  assert.equal(afterPartialWrite.network_telnet_port, "23");
});

test("共享波特率常量、统一握手探测提示文案与终端日志缓冲", async () => {
  assert.deepEqual(
    SERIAL_BAUDRATES,
    [9600, 19200, 38400, 57600, 115200, 230400, 250000, 460800, 921600],
    "全站共享单一波特率列表且包含 19200 与 38400",
  );

  const okProbe = await probeControllerConnection(
    async (_url, payload) => {
      assert.deepEqual(payload, {
        connection_mode: "network",
        network_host: "<device-ip>",
        network_telnet_port: 23,
      });
      return {
        ok: true,
        data: {
          success: true,
          result: { network_connected: true, summary: "FluidNC Idle ($32=1)", machine_limits: { x_accel_mm_s2: 1200 } },
        },
      };
    },
    { connection_mode: "network", network_host: " <device-ip> ", network_telnet_port: "23" },
    12345,
  );
  assert.equal(okProbe.connected, true);
  assert.equal(okProbe.status.checkedAt, 12345);
  assert.match(okProbe.text, /语音和 AI 发送时没指定串口或 IP 就用这个连接/);
  assert.deepEqual(okProbe.machineLimits, { x_accel_mm_s2: 1200 });

  const failProbe = await probeControllerConnection(
    async () => ({ ok: false, data: { success: false, result: "连接被拒绝" } }),
    { connection_mode: "serial", port: "COM5", baudrate: 38400 },
    67890,
  );
  assert.equal(failProbe.connected, false);
  assert.equal(failProbe.status.checkedAt, 67890);
  assert.equal(failProbe.text, "连接被拒绝");

  clearBufferedDeviceLogs();
  emitDeviceLog("DEVICE", "握手探测通过", "ok");
  const logs = readBufferedDeviceLogs();
  assert.equal(logs.length, 1);
  assert.equal(logs[0].tag, "DEVICE");
  assert.equal(logs[0].level, "ok");
  clearBufferedDeviceLogs();
});

test("串口可用性按后端宿主判定：仅 Tauri 安卓本地宿主隐藏串口，安卓浏览器访问电脑端保留串口", async () => {
  const { isTauriHost } = await import("../src/authSession.js");
  const androidNav = {
    userAgent: "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 Chrome/126.0.0.0 Mobile Safari/537.36",
    platform: "Linux armv81",
  };
  const macNav = {
    userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5)",
    platform: "MacIntel",
  };

  // 1. 安卓手机浏览器通过 http://<device-ip>:<port> 访问电脑端：宿主是电脑，必须保留串口
  const androidBrowserWin = { location: { protocol: "http:", hostname: "192.0.2.10", port: "9666" } };
  assert.equal(isTauriHost(androidBrowserWin), false);
  assert.equal(isHostSerialAvailable({ win: androidBrowserWin, nav: androidNav, remote: false }), true);

  // 2. Tauri 安卓 App 本机模式（非连接电脑模式）：本机无 USB 串口后端，隐藏串口
  const tauriAndroidWin = {
    __TAURI__: { core: { invoke: () => {} } },
    location: { protocol: "http:", hostname: "tauri.localhost" },
  };
  assert.equal(isTauriHost(tauriAndroidWin), true);
  assert.equal(isHostSerialAvailable({ win: tauriAndroidWin, nav: androidNav, remote: false }), false);

  // 3. Tauri 安卓 App 开启“连接电脑 MOSS”远程模式：后端在电脑上，保留串口
  assert.equal(isHostSerialAvailable({ win: tauriAndroidWin, nav: androidNav, remote: true }), true);

  // 4. 电脑 Tauri 桌面端：保留串口
  const tauriDesktopWin = {
    __TAURI__: { core: { invoke: () => {} } },
    location: { protocol: "tauri:", hostname: "localhost" },
  };
  assert.equal(isTauriHost(tauriDesktopWin), true);
  assert.equal(isHostSerialAvailable({ win: tauriDesktopWin, nav: macNav, remote: false }), true);
});

test("雕刻机有效行程按连接目标分桶保存：手填优先于固件、再次握手不覆盖手填、支持恢复固件值与 16 目标淘汰", async () => {
  const {
    WORKSPACE_LIMITS_KEY,
    clearManualWorkspaceLimits,
    connectionTargetKey,
    formatWorkspaceLimitsLabel,
    readWorkspaceLimits,
    readWorkspaceLimitsStore,
    resolveWorkspaceLimitsForTarget,
    syncHardwareWorkspaceLimits,
    workspaceLimitsExceedsHardwareWarning,
    writeManualWorkspaceLimits,
  } = await import("../src/deviceConnection.js");
  const store = memoryStorage();
  const targetA = { connection_mode: "network", network_host: "192.0.2.10", network_telnet_port: "23" };
  const targetB = { connection_mode: "serial", port: "COM3", baudrate: "115200" };

  assert.equal(connectionTargetKey(targetA), "network:192.0.2.10:23");
  assert.equal(connectionTargetKey(targetB), "serial:COM3:115200");
  assert.deepEqual(readWorkspaceLimitsStore(store), { targets: {} });
  const initialA = readWorkspaceLimits(store, targetA);
  assert.equal(initialA.x_travel_mm, 100);
  assert.equal(initialA.y_travel_mm, 100);
  assert.deepEqual(initialA.sources, { x: "default", y: "default", z: "default" });
  assert.equal(formatWorkspaceLimitsLabel(initialA), "默认 100");

  // 1. 固件上报 300×200，targetA 用固件值，targetB 仍默认 100
  const hwA = syncHardwareWorkspaceLimits({ x_travel_mm: 300, y_travel_mm: 200 }, targetA, store);
  assert.equal(hwA.x_travel_mm, 300);
  assert.equal(hwA.y_travel_mm, 200);
  assert.deepEqual(hwA.sources, { x: "hardware", y: "hardware", z: "default" });
  assert.equal(formatWorkspaceLimitsLabel(hwA), "固件 300×200");
  assert.equal(workspaceLimitsExceedsHardwareWarning(hwA), "");
  assert.equal(readWorkspaceLimits(store, targetB).x_travel_mm, 100);

  // 2. 用户在 targetA 手填 400×300：手填优先于固件，且提示大于固件行程
  const manA = writeManualWorkspaceLimits({ x_travel_mm: 400, y_travel_mm: 300 }, targetA, store);
  assert.equal(manA.x_travel_mm, 400);
  assert.equal(manA.y_travel_mm, 300);
  assert.deepEqual(manA.sources, { x: "manual", y: "manual", z: "default" });
  assert.equal(manA.has_manual, true);
  assert.equal(formatWorkspaceLimitsLabel(manA), "手填 400×300");
  assert.equal(
    workspaceLimitsExceedsHardwareWarning(manA),
    "大于固件报告的 300×200 mm，发送时仍按固件行程拦截",
  );

  // 3. 再次握手（固件改为 320×200）：只更新 hardware，绝不覆盖用户手填的 400×300
  const reProbedA = syncHardwareWorkspaceLimits({ x_travel_mm: 320, y_travel_mm: 200 }, targetA, store);
  assert.equal(reProbedA.x_travel_mm, 400);
  assert.equal(reProbedA.y_travel_mm, 300);
  assert.deepEqual(reProbedA.hardware, { x: 320, y: 200, z: null });
  assert.equal(
    workspaceLimitsExceedsHardwareWarning(reProbedA),
    "大于固件报告的 320×200 mm，发送时仍按固件行程拦截",
  );

  // 4. 在 targetB 握手单轴（仅 X=260）：targetB 的 Y 回退默认 100，不影响 targetA 的记录
  const singleB = syncHardwareWorkspaceLimits({ x_travel_mm: 260 }, targetB, store);
  assert.equal(singleB.x_travel_mm, 260);
  assert.equal(singleB.y_travel_mm, 100);
  assert.deepEqual(singleB.sources, { x: "hardware", y: "default", z: "default" });
  assert.equal(readWorkspaceLimits(store, targetA).x_travel_mm, 400);

  // 5. 点击“恢复为固件值”：清除 targetA 的 manual，立即恢复为最新固件值 320×200
  const restoredA = clearManualWorkspaceLimits(targetA, store);
  assert.equal(restoredA.x_travel_mm, 320);
  assert.equal(restoredA.y_travel_mm, 200);
  assert.equal(restoredA.has_manual, false);
  assert.deepEqual(restoredA.sources, { x: "hardware", y: "hardware", z: "default" });
  assert.equal(workspaceLimitsExceedsHardwareWarning(restoredA), "");

  // 6. 握手未报任何行程时清空该目标 hardware，回退默认 100
  const clearedB = syncHardwareWorkspaceLimits({ x_accel_mm_s2: 1200 }, targetB, store);
  assert.equal(clearedB.x_travel_mm, 100);
  assert.equal(clearedB.y_travel_mm, 100);
  assert.deepEqual(clearedB.sources, { x: "default", y: "default", z: "default" });

  // 7. 兼容旧单条记录：带 target_key 迁移，不带 target_key 直接丢弃
  const legacyWithKey = memoryStorage();
  legacyWithKey.setItem(
    WORKSPACE_LIMITS_KEY,
    JSON.stringify({ x_travel_mm: 220, y_travel_mm: 180, from_hardware: true, target_key: "serial:COM3:115200" }),
  );
  assert.equal(readWorkspaceLimits(legacyWithKey, targetB).x_travel_mm, 220);
  assert.equal(resolveWorkspaceLimitsForTarget(legacyWithKey, targetA).x_travel_mm, 100);

  const legacyWithoutKey = memoryStorage();
  legacyWithoutKey.setItem(
    WORKSPACE_LIMITS_KEY,
    JSON.stringify({ x_travel_mm: 220, y_travel_mm: 180, from_hardware: true }),
  );
  assert.equal(readWorkspaceLimits(legacyWithoutKey, targetB).x_travel_mm, 100);

  // 8. 最多保留 16 个目标，超出时淘汰最旧的
  const capStore = memoryStorage();
  for (let i = 1; i <= 18; i += 1) {
    syncHardwareWorkspaceLimits(
      { x_travel_mm: 100 + i, y_travel_mm: 100 + i },
      `network:192.0.2.${i}:23`,
      capStore,
    );
  }
  const savedKeys = Object.keys(readWorkspaceLimitsStore(capStore).targets);
  assert.equal(savedKeys.length, 16);
  assert.equal(savedKeys.includes("network:192.0.2.1:23"), false, "最旧目标 #1 应被淘汰");
  assert.equal(savedKeys.includes("network:192.0.2.2:23"), false, "最旧目标 #2 应被淘汰");
  assert.equal(savedKeys.includes("network:192.0.2.18:23"), true);
});
