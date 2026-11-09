import test from "node:test";
import assert from "node:assert/strict";
import { controllerLabel, discoveryOutcome } from "../src/controllerDiscovery.js";

const fluidnc = { ip: "192.0.2.20", telnet_port: 23, hostname: "fluidnc", firmware: "FluidNC v3.7.8", state: "Idle" };
const grbl = { ip: "192.0.2.31", telnet_port: 23, hostname: "", firmware: "Grbl 1.1h", state: "Alarm" };

test("局域网扫描：只找到 1 台才自动填入，多台让用户选，没有时说明原因（2026-11-09）", () => {
  assert.equal(controllerLabel(fluidnc), "192.0.2.20:23 · fluidnc.local · FluidNC v3.7.8 · 空闲");
  assert.equal(controllerLabel(grbl), "192.0.2.31:23 · Grbl 1.1h · 报警锁定");
  assert.equal(
    controllerLabel({ ...fluidnc, machine: "Yisu V1.0", firmware: "FluidNC v4.1.1" }),
    "192.0.2.20:23 · Yisu V1.0 · fluidnc.local · FluidNC v4.1.1 · 空闲",
    "有机器名时放在地址后面，方便区分多台",
  );

  const one = discoveryOutcome({ controllers: [fluidnc], busy: [] });
  assert.equal(one.fill, fluidnc);
  assert.deepEqual(one.choices, []);
  assert.equal(one.log.level, "ok");
  assert.match(one.log.text, /找到 1 台.*已填入.*握手探测/);

  const many = discoveryOutcome({ controllers: [fluidnc, grbl], busy: [{ ip: "192.0.2.40", telnet_port: 23, workflow_id: "wf-1" }] });
  assert.equal(many.fill, null, "多台时不替用户选第一个");
  assert.equal(many.choices.length, 2);
  assert.match(many.log.text, /找到 2 台.*请在下方选择.*另有 1 台正在加工/);

  const none = discoveryOutcome({ controllers: [], busy: [], hint: "检查了 3 个地址，没有设备按 GRBL / FluidNC 回应" });
  assert.equal(none.fill, null);
  assert.equal(none.log.level, "warn");
  assert.equal(none.log.text, "检查了 3 个地址，没有设备按 GRBL / FluidNC 回应");
  assert.equal(discoveryOutcome(null).log.text, "没有设备按 GRBL / FluidNC 回应");
});
