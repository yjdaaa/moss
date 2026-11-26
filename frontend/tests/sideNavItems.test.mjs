// 左侧主导航可见项（2026-11-26 导航减负：摄像头只在内置语音链路显示）。
import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";

const frontendRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

test("摄像头导航只在 MOSS 内置语音链路显示，链路未读到前不显示", async () => {
  const server = await createServer({
    root: frontendRoot,
    configFile: false,
    appType: "custom",
    logLevel: "silent",
    optimizeDeps: { noDiscovery: true },
    server: { middlewareMode: true, hmr: false, watch: null },
  });
  try {
    const { visibleNavItems } = await server.ssrLoadModule("/src/components/SideNav.jsx");
    const labels = (...args) => visibleNavItems(...args).map((item) => item.label);
    assert.deepEqual(labels(false), ["雕刻工作台", "创作画板", "材料实验室", "数字孪生"], "网页 / 手机端没有语音中心与摄像头");
    assert.deepEqual(labels(true, "builtin"), ["雕刻工作台", "创作画板", "材料实验室", "数字孪生", "摄像头", "语音中心"]);
    assert.deepEqual(labels(true, "official"), ["雕刻工作台", "创作画板", "材料实验室", "数字孪生", "语音中心"], "官方链路用不了摄像头，不占导航");
    assert.deepEqual(labels(true, null), ["雕刻工作台", "创作画板", "材料实验室", "数字孪生", "语音中心"], "链路未读到时先不显示");
  } finally {
    await server.close();
  }
});
