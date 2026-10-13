import test from "node:test";
import assert from "node:assert/strict";
import { shouldShowExportSettings } from "../src/exportSettings.js";

test("导出分区只在桌面版 Tauri 显示", () => {
  const tauriWin = { __TAURI__: { core: { invoke: () => {} } } };
  assert.equal(shouldShowExportSettings(tauriWin, { userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Edg/140" }), true);
  assert.equal(shouldShowExportSettings(tauriWin, { userAgent: "Mozilla/5.0 (Linux; Android 14; Pixel) wv" }), false);
  assert.equal(shouldShowExportSettings({}, { userAgent: "Mozilla/5.0 (Windows NT 10.0)" }), false);
  assert.equal(shouldShowExportSettings(undefined, undefined), false);
});
