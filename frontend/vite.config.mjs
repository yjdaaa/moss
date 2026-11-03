import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { resolve } from "node:path";

// The private voice settings page is a packaged Tauri asset, never the
// loopback-hosted MOSS application. Both entries must ship in the same bundle.
export default defineConfig({
  plugins: [react()],
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
