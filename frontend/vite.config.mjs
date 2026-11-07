import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"));
const frontendVersion = (process.env.MOSS_RELEASE_VERSION || "").trim() || pkg.version;

// 2026-11-07（n67）：独立的语音隐私安全窗口页面已取消，只剩主入口。
export default defineConfig({
  plugins: [react()],
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
    },
  },
});
