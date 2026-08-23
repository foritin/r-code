import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 2 前端 Vite 配置
// dev server 端口 5173 与 tauri.conf.json 的 devUrl 对应
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 5173,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2021",
    minify: "esbuild",
    sourcemap: false,
    rollupOptions: {
      output: {
        manualChunks(moduleId) {
          const normalizedId = moduleId.replaceAll("\\", "/");
          if (
            normalizedId.includes("/node_modules/react/")
            || normalizedId.includes("/node_modules/react-dom/")
            || normalizedId.includes("/node_modules/scheduler/")
            || normalizedId.includes("/node_modules/zustand/")
            || normalizedId.includes("/node_modules/use-sync-external-store/")
          ) {
            return "react-vendor";
          }
          if (normalizedId.includes("/node_modules/@tauri-apps/api/")) {
            return "tauri-vendor";
          }
          return undefined;
        },
      },
    },
  },
});
