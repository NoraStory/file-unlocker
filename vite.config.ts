import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";

// Tauri expects a fixed port, fail if that port is not available
export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    // Windows 上 WebView2 是 Chromium，直接用现代 target，产物更小
    target: "es2022",
    minify: "esbuild",
  },
});
