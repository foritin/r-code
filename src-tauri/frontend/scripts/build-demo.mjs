import { access } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { build } from "vite";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const frontendRoot = path.resolve(scriptDir, "..");
const demoDir = path.resolve(frontendRoot, "..", "..", "docs", "ui", "demo");

await build({
  configFile: false,
  root: frontendRoot,
  base: "./",
  plugins: [react()],
  clearScreen: false,
  build: {
    target: "es2021",
    outDir: demoDir,
    emptyOutDir: false,
    minify: "esbuild",
    sourcemap: false,
    cssCodeSplit: false,
    // 离线 file:// 入口必须是一个经典脚本；xterm 也因此内联进同一 bundle。
    chunkSizeWarningLimit: 850,
    modulePreload: false,
    rollupOptions: {
      input: path.resolve(frontendRoot, "src", "demo-main.tsx"),
      output: {
        format: "iife",
        inlineDynamicImports: true,
        entryFileNames: "app.js",
        assetFileNames: (asset) => asset.name?.endsWith(".css") ? "styles.css" : "assets/[name]-[hash][extname]",
      },
    },
  },
});

await Promise.all([
  access(path.join(demoDir, "app.js")),
  access(path.join(demoDir, "styles.css")),
]);

process.stdout.write(`Complete demo built in ${demoDir}\n`);
