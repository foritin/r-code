import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 2 前端 Vite 配置
// dev server 端口 1420 与 tauri.conf.json 的 devUrl 对应
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2021",
    minify: "esbuild",
    sourcemap: false,
  },
});
