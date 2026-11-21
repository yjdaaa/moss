import test from "node:test";
import assert from "node:assert/strict";
import {
  DRAW_CONNECTION_KEYS,
  WORKBENCH_CONNECTION_KEYS,
  readConnectionForm,
  readRawConnection,
  readWorkbenchConnection,
  writeWorkbenchConnection,
} from "../src/deviceConnection.js";

function memoryStorage(initial = {}) {
  const data = new Map(Object.entries(initial));
  return {
    data,
    getItem: (key) => (data.has(key) ? data.get(key) : null),
    setItem: (key, value) => data.set(key, String(value)),
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
