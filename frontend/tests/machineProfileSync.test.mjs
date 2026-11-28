import test from "node:test";
import assert from "node:assert/strict";
import {
  WORKSPACE_LIMITS_KEY,
  readRawLocalWorkspaceLimitsStore,
  readWorkspaceLimitsStore,
  resolveWorkspaceLimitsForTarget,
  syncHardwareWorkspaceLimits,
} from "../src/deviceConnection.js";
import {
  FILL_SETTINGS_KEY,
  MACHINE_MOTION_KEY,
  readMachineMotion,
  readRawLocalMachineMotionStore,
  writeMachineMotion,
} from "../src/fillOptions.js";
import { FRAMING_SHAPE_KEY, readFramingShape } from "../src/aiPreviewState.js";
import {
  applyMachineCalibrationToFillSettings,
  canEditMachineProfiles,
  extractLegacyMigrationPayload,
  getRuntimeMachineState,
  hydrateMachineProfilesFromSystemInfo,
  resetManualWorkspaceLimits,
  resetRuntimeMachineState,
  saveFramingShapeSetting,
  saveManualWorkspaceLimits,
  saveTargetMachineCalibration,
} from "../src/machineProfileSync.js";

function memoryStorage(initial = {}) {
  const data = new Map(Object.entries(initial));
  return {
    getItem(key) {
      return data.has(key) ? data.get(key) : null;
    },
    setItem(key, value) {
      data.set(key, String(value));
    },
    removeItem(key) {
      data.delete(key);
    },
    dump() {
      return Object.fromEntries(data.entries());
    },
  };
}

function withFakeGlobals(storage, fn) {
  const prevStorage = globalThis.localStorage;
  const prevWindow = globalThis.window;
  const events = [];
  globalThis.localStorage = storage;
  globalThis.window = {
    dispatchEvent(evt) {
      events.push({ type: evt.type, detail: evt.detail });
    },
  };
  return Promise.resolve()
    .then(() => fn(events))
    .finally(() => {
      resetRuntimeMachineState();
      if (prevStorage === undefined) delete globalThis.localStorage;
      else globalThis.localStorage = prevStorage;
      if (prevWindow === undefined) delete globalThis.window;
      else globalThis.window = prevWindow;
    });
}

test("r78 迁移：本地有旧手填值且后端为空时一次性迁移并清理本地手填，保留硬件缓存与界面草稿", async () => {
  const target = { connection_mode: "network", network_host: "10.0.0.5", network_telnet_port: "23" };
  const key = "network:10.0.0.5:23";
  const storage = memoryStorage({
    "laser-forge-conn-mode": "network",
    "laser-forge-conn-host": "10.0.0.5",
    "laser-forge-conn-port": "23",
    [WORKSPACE_LIMITS_KEY]: JSON.stringify({
      targets: {
        [key]: {
          hardware: { x: 100, y: 100 },
          manual: { x: 300, y: 200 },
          identity: { firmware: "FluidNC 4.1", machine_name: "Yisu" },
          updated_at: 10,
        },
      },
    }),
    [MACHINE_MOTION_KEY]: JSON.stringify({
      targets: {
        [key]: {
          motion: { x_accel_mm_s2: 1200, y_accel_mm_s2: 1200 },
          updated_at: 10,
        },
      },
    }),
    [FILL_SETTINGS_KEY]: JSON.stringify({
      angleDeg: "45",
      kerfMm: "0.12",
      accelerationMmS2: "500",
      scanOffsetMm: "0.18",
      scanOffsetTable: [{ speed: "2000", offset: "0.24" }],
    }),
    [FRAMING_SHAPE_KEY]: "hull",
  });

  // 确认不会把固件 motion 当手填上传
  const previewPlan = extractLegacyMigrationPayload(storage);
  assert.equal(previewPlan.hasWorkspaceManual, true);
  assert.equal(previewPlan.hasMotionManual, false);
  assert.equal(previewPlan.hasFillCalibration, true);
  assert.equal(previewPlan.hasFramingShape, true);
  assert.equal(previewPlan.machine_profiles_migrate.targets[key].manual_travel.x_travel_mm, 300);
  assert.equal(previewPlan.machine_profiles_migrate.targets[key].manual_motion.x_accel_mm_s2, 500);
  assert.equal(previewPlan.machine_profiles_migrate.targets[key].scan_offset_mm, 0.18);
  assert.deepEqual(previewPlan.machine_profiles_migrate.targets[key].scan_offset_table, [[2000, 0.24]]);
  assert.equal(previewPlan.laser_framing_shape_migrate, "hull");

  const posted = [];
  await withFakeGlobals(storage, async () => {
    await hydrateMachineProfilesFromSystemInfo(
      {
        can_edit_operator_settings: true,
        machine_profiles: { targets: {} },
        laser_framing_shape: "box",
      },
      {
        storage,
        post: async (path, body) => {
          posted.push({ path, body });
          return {
            ok: true,
            status: 200,
            data: {
              success: true,
              result: {
                machine_profiles: body.machine_profiles_migrate,
                laser_framing_shape: body.laser_framing_shape_migrate,
              },
            },
          };
        },
      },
    );

    assert.equal(posted.length, 1);
    assert.equal(posted[0].path, "/api/settings");

    // 迁移后本地已清理手填值，但保留硬件缓存与非机器草稿
    const rawWs = readRawLocalWorkspaceLimitsStore(storage);
    assert.deepEqual(rawWs.targets[key].hardware, { x: 100, y: 100 });
    assert.deepEqual(rawWs.targets[key].manual, {});
    assert.equal(rawWs.targets[key].identity.machine_name, "Yisu");
    assert.equal(storage.getItem(FRAMING_SHAPE_KEY), null);
    const localFill = JSON.parse(storage.getItem(FILL_SETTINGS_KEY));
    assert.equal(localFill.angleDeg, "45");
    assert.equal(localFill.kerfMm, "0.12");
    assert.equal(localFill.accelerationMmS2, "");
    assert.equal(localFill.scanOffsetMm, "0");
    assert.deepEqual(localFill.scanOffsetTable, []);

    // 默认读入口合并后端权威态
    const resolved = resolveWorkspaceLimitsForTarget(readWorkspaceLimitsStore(), target);
    assert.equal(resolved.x_travel_mm, 300);
    assert.equal(resolved.sources.x, "manual");
    assert.equal(resolved.hardware.x, 100);
    assert.equal(readMachineMotion(target).x_accel_mm_s2, 500);
    assert.equal(readFramingShape(), "hull");
  });
});

test("r78 迁移：后端已有值时以后端为准不覆盖；局域网只读端跳过迁移且保留只读态", async () => {
  const key = "network:10.0.0.8:23";
  const storage = memoryStorage({
    [WORKSPACE_LIMITS_KEY]: JSON.stringify({
      targets: { [key]: { hardware: {}, manual: { x: 400 }, updated_at: 1 } },
    }),
    [FRAMING_SHAPE_KEY]: "box",
  });

  // 1. 局域网只读客户端：不 POST 迁移、不删本地、呈现后端权威值
  let postCalled = 0;
  await withFakeGlobals(storage, async () => {
    await hydrateMachineProfilesFromSystemInfo(
      {
        can_edit_operator_settings: false,
        machine_profiles: {
          targets: {
            [key]: { manual_travel: { x_travel_mm: 250, y_travel_mm: 210 }, updated_at: 20 },
          },
        },
        laser_framing_shape: "hull",
      },
      {
        storage,
        post: async () => {
          postCalled += 1;
          return { ok: false, status: 403 };
        },
      },
    );
    assert.equal(postCalled, 0);
    assert.equal(canEditMachineProfiles(), false);
    assert.equal(readFramingShape(), "hull");
    const resolved = resolveWorkspaceLimitsForTarget(readWorkspaceLimitsStore(), key);
    assert.equal(resolved.x_travel_mm, 250);
    // 本地旧记录未被只读端破坏
    assert.equal(readRawLocalWorkspaceLimitsStore(storage).targets[key].manual.x, 400);
  });

  // 2. 两边都有值时，可写端把本地发给后端 migrate，但最终采用后端返回的合并结果（后端已有值胜出）
  await withFakeGlobals(storage, async () => {
    await hydrateMachineProfilesFromSystemInfo(
      {
        can_edit_operator_settings: true,
        machine_profiles: {
          targets: {
            [key]: { manual_travel: { x_travel_mm: 250 }, updated_at: 20 },
          },
        },
        laser_framing_shape: "hull",
      },
      {
        storage,
        post: async (_path, body) => {
          assert.equal(body.machine_profiles_migrate.targets[key].manual_travel.x_travel_mm, 400);
          assert.equal(body.laser_framing_shape_migrate, "box");
          return {
            ok: true,
            status: 200,
            data: {
              success: true,
              result: {
                machine_profiles: {
                  targets: {
                    [key]: { manual_travel: { x_travel_mm: 250 }, updated_at: 20 },
                  },
                },
                laser_framing_shape: "hull",
              },
            },
          };
        },
      },
    );
    assert.equal(canEditMachineProfiles(), true);
    assert.equal(resolveWorkspaceLimitsForTarget(readWorkspaceLimitsStore(), key).x_travel_mm, 250);
    assert.equal(readFramingShape(), "hull");
    assert.equal(readRawLocalWorkspaceLimitsStore(storage).targets[key], undefined);
  });
});

test("r78 读写、失败回滚、按目标切换校准，且硬件握手不覆写后端手填", async () => {
  const targetA = { connection_mode: "network", network_host: "10.0.0.1", network_telnet_port: "23" };
  const targetB = { connection_mode: "serial", port: "COM3", baudrate: "115200" };
  const keyA = "network:10.0.0.1:23";
  const keyB = "serial:COM3:115200";
  const storage = memoryStorage();

  await withFakeGlobals(storage, async (events) => {
    await hydrateMachineProfilesFromSystemInfo(
      {
        can_edit_operator_settings: true,
        machine_profiles: {
          targets: {
            [keyA]: {
              manual_travel: { x_travel_mm: 280, y_travel_mm: 190 },
              manual_motion: { x_accel_mm_s2: 450, y_accel_mm_s2: 450 },
              scan_offset_mm: 0.14,
              updated_at: 10,
            },
            [keyB]: {
              scan_offset_table: [[1500, 0.22]],
              updated_at: 11,
            },
          },
        },
        laser_framing_shape: "box",
      },
      { storage, post: async () => ({ ok: true, data: { success: true, result: {} } }) },
    );

    // 硬件握手写入 localStorage 的 hardware 槽位时，不得丢失或反向写回后端 manual_travel
    const synced = syncHardwareWorkspaceLimits({ x_travel_mm: 100, y_travel_mm: 100 }, targetA);
    assert.equal(synced.x_travel_mm, 280);
    assert.equal(synced.sources.x, "manual");
    assert.equal(synced.hardware.x, 100);
    assert.deepEqual(readRawLocalWorkspaceLimitsStore(storage).targets[keyA].manual, {});

    // 硬件写入固件加速度 900 时，localStorage 只存固件值，但默认读取仍保留后端手填 450
    writeMachineMotion({ x_accel_mm_s2: 900, y_accel_mm_s2: 900 }, targetA);
    assert.equal(readRawLocalMachineMotionStore(storage).targets[keyA].motion.x_accel_mm_s2, 900);
    assert.equal(readMachineMotion(targetA).x_accel_mm_s2, 450);

    // 切换目标自动切到对应目标的扫描偏移与加速度
    const fillA = applyMachineCalibrationToFillSettings({}, getRuntimeMachineState().profiles, targetA);
    const fillB = applyMachineCalibrationToFillSettings({}, getRuntimeMachineState().profiles, targetB);
    assert.equal(fillA.accelerationMmS2, "450");
    assert.equal(fillA.scanOffsetMm, "0.14");
    assert.equal(fillB.accelerationMmS2, "");
    assert.deepEqual(fillB.scanOffsetTable, [{ speed: "1500", offset: "0.22" }]);

    // 保存失败（403）时回滚到原值
    const failSave = await saveManualWorkspaceLimits(
      { x_travel_mm: 500 },
      targetA,
      { post: async () => ({ ok: false, status: 403, data: { result: "仅主窗口可改" } }) },
    );
    assert.equal(failSave.ok, false);
    assert.equal(resolveWorkspaceLimitsForTarget(readWorkspaceLimitsStore(), targetA).x_travel_mm, 280);

    const failShape = await saveFramingShapeSetting("hull", {
      post: async () => ({ ok: false, status: 403, data: { result: "只读" } }),
    });
    assert.equal(failShape.ok, false);
    assert.equal(readFramingShape(), "box");

    // 正常写入扫描偏移、恢复固件行程、保存走边框形状
    const okCal = await saveTargetMachineCalibration(
      { accelerationMmS2: "600", scanOffsetMm: "0.2", scanOffsetTable: [{ speed: "1000", offset: "0.1" }] },
      targetA,
      {
        post: async (_p, body) => ({
          ok: true,
          status: 200,
          data: {
            success: true,
            result: {
              machine_profiles: {
                targets: {
                  [keyA]: {
                    manual_travel: { x_travel_mm: 280, y_travel_mm: 190 },
                    manual_motion: { x_accel_mm_s2: 600, y_accel_mm_s2: 600 },
                    scan_offset_mm: 0.2,
                    scan_offset_table: body.machine_profile_patch.scan_offset_table,
                    updated_at: 30,
                  },
                },
              },
              laser_framing_shape: "box",
            },
          },
        }),
      },
    );
    assert.equal(okCal.ok, true);
    assert.equal(readMachineMotion(targetA).x_accel_mm_s2, 600);

    const okReset = await resetManualWorkspaceLimits(targetA, {
      post: async () => ({
        ok: true,
        status: 200,
        data: {
          success: true,
          result: {
            machine_profiles: { targets: {} },
            laser_framing_shape: "box",
          },
        },
      }),
    });
    assert.equal(okReset.ok, true);
    const afterReset = resolveWorkspaceLimitsForTarget(readWorkspaceLimitsStore(), targetA);
    assert.equal(afterReset.x_travel_mm, 100);
    assert.equal(afterReset.sources.x, "hardware");

    const okShape = await saveFramingShapeSetting("hull", {
      post: async () => ({
        ok: true,
        status: 200,
        data: { success: true, result: { machine_profiles: { targets: {} }, laser_framing_shape: "hull" } },
      }),
    });
    assert.equal(okShape.ok, true);
    assert.equal(readFramingShape(), "hull");
    assert.ok(events.some((e) => e.type === "moss:framing-shape" && e.detail.shape === "hull"));
  });
});
