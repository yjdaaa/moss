import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"));
const frontendVersion = (process.env.MOSS_RELEASE_VERSION || "").trim() || pkg.version;

// The private voice settings page is a packaged Tauri asset, never the
// loopback-hosted MOSS application. Both entries must ship in the same bundle.
export default defineConfig({
  plugins: [react()],
  define: {
    __MOSS_FRONTEND_VERSION__: JSON.stringify(frontendVersion),
  },
  optimizeDeps: {
    entries: ["index.html", "voice-private.html"],
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
        "voice-private": resolve(process.cwd(), "voice-private.html"),
      },
    },
  },
});
