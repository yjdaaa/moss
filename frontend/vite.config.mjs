import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"));
const frontendVersion = (process.env.MOSS_RELEASE_VERSION || "").trim() || pkg.version;

const EXCALIDRAW_LOCALE_STUB = `
export const alerts = {}, buttons = {}, canvasError = {}, clearCanvasDialog = {}, colorPicker = {},
  colors = {}, confirmDialog = {}, encrypted = {}, errorDialog = {}, errorSplash = {}, errors = {},
  exportDialog = {}, headings = {}, helpDialog = {}, hints = {}, imageExportDialog = {}, labels = {},
  library = {}, mermaid = {}, overwriteConfirm = {}, publishDialog = {}, publishSuccessDialog = {},
  roomDialog = {}, stats = {}, toast = {}, toolBar = {}, welcomeScreen = {};
export default {};
`;

// 画板固定使用中文（回退英文）；将 Excalidraw 约 60 个未使用的外语包替换为轻量空模块，省去内嵌产物体积。
function stubUnusedExcalidrawLocales() {
  return {
    name: "moss-stub-unused-excalidraw-locales",
    load(id) {
      const normalized = id.replace(/\\/g, "/");
      const marker = "/@excalidraw/excalidraw/dist/prod/locales/";
      const idx = normalized.indexOf(marker);
      if (idx === -1) return null;
      const filename = normalized.slice(idx + marker.length).split("?")[0];
      if (/^(en-|zh-|percentages-)/.test(filename)) return null;
      return EXCALIDRAW_LOCALE_STUB;
    },
  };
}

// 2026-11-07（n67）：独立的语音隐私安全窗口页面已取消，只剩主入口。
export default defineConfig({
  plugins: [react(), stubUnusedExcalidrawLocales()],
  define: {
    __MOSS_FRONTEND_VERSION__: JSON.stringify(frontendVersion),
  },
  optimizeDeps: {
    entries: ["index.html"],
  },
  server: {
    host: "127.0.0.1",
    port: 5177,
    strictPort: true,
  },
  build: {
    rollupOptions: {
      input: {
        main: resolve(process.cwd(), "index.html"),
      },
      output: {
        manualChunks(id) {
          if (id.includes("/node_modules/three/") || id.includes("\\node_modules\\three\\")) {
            return "vendor-three";
          }
          return undefined;
        },
      },
    },
  },
});
