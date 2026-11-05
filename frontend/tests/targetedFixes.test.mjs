import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { formatFramingStatus } from "../src/drawConnection.js";

const testsDir = path.dirname(fileURLToPath(import.meta.url));
const srcRoot = path.resolve(testsDir, "../src");

test("DrawPage 统一复用 client.js postJson 契约，走边框成功不再误报未知错误", () => {
  const drawSource = readFileSync(path.join(srcRoot, "pages/DrawPage.jsx"), "utf8");
  assert.doesNotMatch(drawSource, /async\s+function\s+postJson\s*\(/, "不得再维护重复解包版 postJson");
  assert.match(drawSource, /import\s*\{[^}]*\bpostJson\b[^}]*\}\s*from\s*["']\.\.\/api\/client\.js["']/);

  assert.equal(
    formatFramingStatus({ status: 200, ok: true, data: { success: true, result: { framed: true } } }, 80, 50, false),
    "走边框完成 (80.0 x 50.0 mm，未出光)",
  );
  assert.equal(
    formatFramingStatus({ status: 200, ok: true, data: { success: true } }, 45.25, 30, true),
    "走边框完成 (45.3 x 30.0 mm，弱光)",
  );
  assert.equal(
    formatFramingStatus({ status: 400, ok: false, data: { success: false, result: "控制器忙" } }, 80, 50, false),
    "走边框未完成：控制器忙",
  );
});

test("VoiceAgentCard 5 秒轮询保脏、VoicePage 链路提示、HomePage 预览不漏 Blob、MaterialModal 多厚度保存与整材删除", async () => {
  const voicePageSource = readFileSync(path.join(srcRoot, "pages/VoicePage.jsx"), "utf8");
  assert.doesNotMatch(voicePageSource, /第三方工具（与链路无关）/);
  assert.match(voicePageSource, /第三方扩展与本机代理（内置服务链路调用）/);
  assert.match(voicePageSource, /第三方 MCP 工具与本机 Agent 仅在“MOSS 内置服务”链路下对语音开放/);

  const approvalSource = readFileSync(path.join(srcRoot, "components/ApprovalCenter.jsx"), "utf8");
  assert.match(approvalSource, /approval-agent-preview/);
  const dockSource = readFileSync(path.join(srcRoot, "components/JobProgressDock.jsx"), "utf8");
  assert.match(dockSource, /\/api\/jobs\/acknowledge-stop/);
  assert.match(dockSource, /已确认停稳/);

  const server = await createServer({
    configFile: false,
    appType: "custom",
    logLevel: "silent",
    optimizeDeps: { noDiscovery: true },
    server: { middlewareMode: true, hmr: false, watch: null },
  });
  try {
    const { shouldSyncAgentFormOnPoll } = await server.ssrLoadModule("/src/components/VoiceAgentCard.jsx");
    const { loadWorkbenchPreviewUrl } = await server.ssrLoadModule("/src/pages/HomePage.jsx");
    const {
      parseThicknessList,
      buildMaterialSavePayload,
      buildMaterialDeletePayload,
    } = await server.ssrLoadModule("/src/components/MaterialModal.jsx");

    const saved = { enabled: false, workspace: "", codex_executable: "", claude_executable: "" };
    assert.equal(
      shouldSyncAgentFormOnPoll({ loaded: false, saving: false, saved, config: saved }),
      true,
      "首次加载允许填入初始配置",
    );
    assert.equal(
      shouldSyncAgentFormOnPoll({ loaded: true, saving: false, saved, config: { ...saved } }),
      true,
      "未修改时轮询可同步配置",
    );
    assert.equal(
      shouldSyncAgentFormOnPoll({
        loaded: true,
        saving: false,
        saved,
        config: { ...saved, enabled: true, workspace: "C:\\work" },
      }),
      false,
      "存在未保存输入时 5 秒轮询不得覆盖表单",
    );
    assert.equal(
      shouldSyncAgentFormOnPoll({ loaded: true, saving: true, saved, config: { ...saved } }),
      false,
      "保存请求进行中时轮询不得覆盖表单",
    );

    // 乱序时序：轮询 A（pollEpoch=1）在保存 B（使 currentEpoch 升至 3）开始前发出，
    // 保存 B 完成后即使 saving=false 且 config===saved（表单已不脏），旧轮询 A 返回也必须被拒绝回滚
    const savedB = { ...saved, enabled: true, workspace: "C:\\project-b" };
    assert.equal(
      shouldSyncAgentFormOnPoll({
        pollEpoch: 1,
        currentEpoch: 3,
        loaded: true,
        saving: false,
        saved: savedB,
        config: { ...savedB },
      }),
      false,
      "保存前发起的过期轮询响应绝不能覆盖刚保存成功的新配置",
    );
    assert.equal(
      shouldSyncAgentFormOnPoll({
        pollEpoch: 3,
        currentEpoch: 3,
        loaded: true,
        saving: false,
        saved: savedB,
        config: { ...savedB },
      }),
      true,
      "保存完成后新发起的同世代轮询正常同步",
    );

    let blobFetches = 0;
    const localUrl = await loadWorkbenchPreviewUrl("out/preview 1.png", {
      remote: false,
      fetchBlobUrl: async () => {
        blobFetches += 1;
        return "blob:http://127.0.0.1/unused";
      },
      resolveUrl: (p) => `http://127.0.0.1:9666${p}`,
    });
    assert.equal(blobFetches, 0, "桌面/本机模式不创建未释放的 blob URL");
    assert.equal(localUrl, "http://127.0.0.1:9666/preview?path=out%2Fpreview%201.png");

    const remoteUrl = await loadWorkbenchPreviewUrl("out/preview 1.png", {
      remote: true,
      fetchBlobUrl: async (file) => {
        blobFetches += 1;
        assert.equal(file, "out/preview 1.png");
        return "blob:http://127.0.0.1/auth-preview";
      },
      resolveUrl: (p) => p,
    });
    assert.equal(blobFetches, 1);
    assert.equal(remoteUrl, "blob:http://127.0.0.1/auth-preview");

    const thickList = parseThicknessList("3, 5，8");
    assert.deepEqual(thickList, [3, 5, 8]);

    const outlineSave = buildMaterialSavePayload({
      material: "椴木",
      thickList,
      power: 60,
      speed: 25,
      laserMode: "engrave",
      engravingMode: "outline",
    });
    assert.equal(outlineSave.thickness_mm, 3);
    assert.equal(outlineSave.laser_max_power, 600);
    assert.equal(outlineSave.feed_rate, 1500);
    assert.deepEqual(Object.keys(outlineSave.thicknesses), ["3", "5", "8"]);
    assert.deepEqual(outlineSave.thicknesses["5"], {
      engrave: {
        outline: {
          laser_min_power: 0,
          laser_max_power: 600,
          feed_rate: 1500,
          travel_rate: 3000,
          threshold: -1,
          passes: 1,
          pixel_size_mm: 0.1,
        },
      },
    });

    const cutSave = buildMaterialSavePayload({
      material: "亚克力",
      thickList: [3, 5],
      power: 85,
      speed: 10,
      laserMode: "cut",
      engravingMode: "cut",
    });
    assert.deepEqual(cutSave.thicknesses["3"], {
      cut: {
        laser_min_power: 0,
        laser_max_power: 850,
        feed_rate: 600,
        travel_rate: 3000,
        threshold: -1,
        passes: 1,
        pixel_size_mm: 0.1,
      },
    });

    assert.deepEqual(buildMaterialDeletePayload("椴木"), {
      action: "delete",
      material: "椴木",
      thickness_mm: 0,
      confirmed: true,
    });
  } finally {
    await server.close();
  }
});

test("工作台预览：不伪造 SHA-256、不跳过 HTTP 状态检查，统一走 fetchPreviewText", () => {
  const home = readFileSync(path.join(srcRoot, "pages/HomePage.jsx"), "utf8");
  assert.doesNotMatch(home, /d8f2b1a9/, "不得用假摘要兜底");
  assert.doesNotMatch(home, /preview\?path=\$\{encodeURIComponent\(artifacts\.gcode_file\)\}/, "G-code 预览统一走 fetchPreviewText");
  assert.match(home, /fetchPreviewText\(artifacts\.gcode_file\)/);
  assert.match(home, /摘要不可用/);
  const twin = readFileSync(path.join(srcRoot, "pages/TwinPage.jsx"), "utf8");
  assert.match(twin, /fetchPreviewText\(gcodeFile\)/);
  assert.match(twin, /seq !== workflowLoadSeqRef\.current/, "旧任务刀路响应不得覆盖新选中的任务");
});

test("三个页面共用 usePageEmergencyStop 两段式急停；走边框先确认，急停路径不加确认", () => {
  for (const file of ["pages/HomePage.jsx", "pages/DrawPage.jsx", "pages/TwinPage.jsx"]) {
    const source = readFileSync(path.join(srcRoot, file), "utf8");
    assert.match(source, /usePageEmergencyStop\(/, `${file} 必须复用共享页面急停`);
    assert.doesNotMatch(source, /onEmergencyStop\(/, `${file} 不得自行再注册一份急停监听`);
  }
  const hook = readFileSync(path.join(srcRoot, "usePageEstop.js"), "utf8");
  assert.doesNotMatch(hook, /showConfirm/, "急停路径不得增加确认框");
  const draw = readFileSync(path.join(srcRoot, "pages/DrawPage.jsx"), "utf8");
  const framing = draw.slice(draw.indexOf("async function runFraming()"), draw.indexOf("async function testConnection()"));
  assert.ok(framing.indexOf("showConfirm(") > 0 && framing.indexOf("showConfirm(") < framing.indexOf('postJson("/api/device/framing"'), "走边框必须先确认再下发");
  assert.match(framing, /if \(!confirmed\)/);
  assert.doesNotMatch(draw, /^const DEFAULT_FORM = \{[\s\S]*?localStorage\.getItem/m, "模块顶层不得直接读 localStorage");
});
