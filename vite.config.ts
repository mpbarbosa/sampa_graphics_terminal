import { defineConfig } from "vite";

// Vite serves the frontend; Tauri wraps it in a native window. The dev server
// must use a fixed port that matches `devUrl` in src-tauri/tauri.conf.json.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    // Tauri (webkit2gtk / WebView2) targets modern engines; allow top-level await.
    target: "esnext",
  },
});
