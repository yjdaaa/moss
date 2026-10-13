import { defineConfig } from "vite";
import { resolve } from "node:path";

// The private voice settings page is a packaged Tauri asset, never the
// loopback-hosted MOSS application. Both entries must ship in the same bundle.
export default defineConfig({
  build: {
    rollupOptions: {
      input: {
        main: resolve(process.cwd(), "index.html"),
        "voice-private": resolve(process.cwd(), "voice-private.html"),
      },
    },
  },
});
