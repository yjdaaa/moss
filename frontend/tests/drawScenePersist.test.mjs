import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import {
  drawSceneChanges,
  drawSceneToRestore,
  planDrawHydration,
  snapshotDrawScene,
  takePendingDrawScene,
  shouldApplyRestoredPreview,
  shouldPersistLastPreview,
} from "../src/drawScenePersist.js";

test("#23 画板恢复查询未落定前不清空指针，迟到响应不覆写用户新建/采纳的预览", () => {
  assert.equal(
    shouldPersistLastPreview({ workflowId: "", previewSnap: null, lastPreviewHydrated: false }),
    false,
    "挂载恢复请求尚未返回前，不得因初始空值把旧指针写空",
  );
  assert.equal(
    shouldPersistLastPreview({ workflowId: "", previewSnap: null, lastPreviewHydrated: true }),
    true,
    "恢复完成且确实无有效预览后，允许清理无效指针",
  );
  assert.equal(
    shouldPersistLastPreview({
      workflowId: "wf-new",
      previewSnap: { sceneRev: "a:1:1", formSig: "{}" },
      lastPreviewHydrated: false,
    }),
    true,
    "用户主动生成或采纳了新预览后，立即允许持久化",
  );

  assert.equal(
    shouldApplyRestoredPreview({ cancelled: false, requestSeq: 0, currentSeq: 1 }),
    false,
    "用户已发起新预览生成/采纳（seq 推进），迟到的旧恢复查询必须丢弃",
  );
  assert.equal(
    shouldApplyRestoredPreview({ cancelled: true, requestSeq: 1, currentSeq: 1 }),
    false,
  );
  assert.equal(
    shouldApplyRestoredPreview({ cancelled: false, requestSeq: 2, currentSeq: 2 }),
    true,
  );
});

test("#24 桌面端口切换时按后端数据目录恢复画板，首次迁移从本地回填，并发改动不被旧后端覆盖", () => {
  const migrated = planDrawHydration({
    remoteState: { exists: false, scene: null, last_preview: null, form: {}, last_accepted_fingerprint: null },
    localSceneRaw: "{\"version\":1,\"elements\":[{\"id\":\"e1\"}]}",
    localLastPreviewRaw: "{\"workflowId\":\"wf-local\",\"sceneRev\":\"e1:1:1\",\"formSig\":\"{}\"}",
    localForm: { material: "椴木", width_mm: "80" },
    localFingerprint: "a".repeat(64),
    configKeys: ["material", "width_mm", "height_mm"],
  });
  assert.equal(migrated.sceneToApply, null);
  assert.equal(migrated.lastPreviewRaw, "{\"workflowId\":\"wf-local\",\"sceneRev\":\"e1:1:1\",\"formSig\":\"{}\"}");
  assert.deepEqual(migrated.backfill, {
    scene: "{\"version\":1,\"elements\":[{\"id\":\"e1\"}]}",
    last_preview: "{\"workflowId\":\"wf-local\",\"sceneRev\":\"e1:1:1\",\"formSig\":\"{}\"}",
    form: { material: "椴木", width_mm: "80" },
    last_accepted_fingerprint: "a".repeat(64),
  });

  const switchedPort = planDrawHydration({
    remoteState: {
      exists: true,
      scene: "{\"version\":1,\"elements\":[{\"id\":\"e-remote\"}]}",
      last_preview: "{\"workflowId\":\"wf-remote\",\"sceneRev\":\"e-remote:1:1\",\"formSig\":\"{}\"}",
      form: { material: "亚克力", width_mm: "120" },
      last_accepted_fingerprint: "b".repeat(64),
    },
    localSceneRaw: null,
    localLastPreviewRaw: null,
    localForm: {},
    localFingerprint: "",
    configKeys: ["material", "width_mm", "height_mm"],
    sceneDirty: false,
    dirtyFormKeys: new Set(["width_mm"]),
    previewActionSeq: 0,
  });
  assert.equal(switchedPort.sceneToApply, "{\"version\":1,\"elements\":[{\"id\":\"e-remote\"}]}");
  assert.equal(switchedPort.lastPreviewRaw, "{\"workflowId\":\"wf-remote\",\"sceneRev\":\"e-remote:1:1\",\"formSig\":\"{}\"}");
  assert.deepEqual(switchedPort.formUpdates, { material: "亚克力" }, "用户已改过的字段不被远端恢复覆写");
  assert.equal(switchedPort.fingerprintUpdate, "b".repeat(64));
  assert.deepEqual(switchedPort.backfill, {});

  const raced = planDrawHydration({
    remoteState: {
      exists: true,
      scene: "{\"version\":1,\"elements\":[{\"id\":\"e-stale\"}]}",
      last_preview: "{\"workflowId\":\"wf-stale\",\"sceneRev\":\"r\",\"formSig\":\"s\"}",
      form: { material: "皮革" },
    },
    configKeys: ["material"],
    sceneDirty: true,
    dirtyFormKeys: new Set(["material"]),
    previewActionSeq: 1,
  });
  assert.equal(raced.sceneToApply, null, "恢复期间画布已改动，不回盖");
  assert.equal(raced.lastPreviewRaw, null, "恢复期间用户已新建预览，不回盖");
  assert.deepEqual(raced.formUpdates, {});
});

test("#23/#24 DrawPage 接线守护：接入后端 /api/draw/state 且恢复未落定不写空指针", () => {
  const src = readFileSync(new URL("../src/pages/DrawPage.jsx", import.meta.url), "utf8");
  assert.match(src, /\/api\/draw\/state/, "DrawPage 必须通过同源后端 /api/draw/state 持久化画板状态");
  assert.match(src, /shouldPersistLastPreview\(/, "写入上次预览前必须经过 shouldPersistLastPreview");
  assert.match(src, /shouldApplyRestoredPreview\(/, "应用恢复查询结果前必须经过 shouldApplyRestoredPreview");
});

test("后端空画布仍是待应用场景，挂载完成后清掉旧 localStorage 画布", () => {
  const old = JSON.stringify({ version: 1, elements: [{ id: "local" }] });
  const plan = planDrawHydration({ remoteState: { scene: "" }, localSceneRaw: old });
  const pending = drawSceneToRestore(plan.sceneToApply);
  assert.equal(takePendingDrawScene(pending, { apiReady: true, canvasReady: false }), null);
  assert.deepEqual(takePendingDrawScene(pending, { apiReady: true, canvasReady: true }),
    { elements: [], appState: {}, files: {} });
  assert.deepEqual(plan.backfill, {});
  assert.equal(drawSceneToRestore(null), null);
  assert.equal(snapshotDrawScene(pending.elements).sceneRev, "empty");
});

test("画板同步只随元素修订或实际图片内容变化，滚动和选择不上传", () => {
  const elements = [{ id: "img", version: 1, versionNonce: 3, fileId: "asset" }];
  const files = { asset: { dataURL: "data:image/png;base64,AA==", mimeType: "image/png" } };
  const initial = snapshotDrawScene(elements, { scrollX: 0 }, files);
  assert.deepEqual(drawSceneChanges(initial, snapshotDrawScene(elements, { scrollX: 20 }, files)),
    { content: false, view: true });
  assert.deepEqual(drawSceneChanges(initial, snapshotDrawScene(elements, { scrollX: 0, selectedElementIds: { img: true } }, files)),
    { content: false, view: false });
  assert.equal(drawSceneChanges(initial, snapshotDrawScene([{ ...elements[0], version: 2 }], {}, files)).content, true);
  assert.equal(drawSceneChanges(initial, snapshotDrawScene(elements, {}, { asset: { ...files.asset, dataURL: "data:image/png;base64,BB==" } })).content, true);
  assert.equal(drawSceneChanges(initial, snapshotDrawScene(elements, {}, {})).content, true);
  assert.equal(drawSceneChanges(initial, snapshotDrawScene([], {}, files)).content, true);
});

test("重新打开时保留同一内容的本地视图，远端清空仍优先", () => {
  const make = (version, scrollX) => JSON.stringify({ version: 1,
    elements: [{ id: "a", version }], appState: { scrollX } });
  const localSceneRaw = make(1, 12);
  assert.equal(planDrawHydration({ localSceneRaw, remoteState: { scene: make(1, 0) } }).sceneToApply, localSceneRaw);
  assert.equal(planDrawHydration({ localSceneRaw, remoteState: { scene: make(2, 0) } }).sceneToApply, make(2, 0));
  assert.equal(planDrawHydration({ localSceneRaw, remoteState: { scene: "" } }).sceneToApply, "");
});
