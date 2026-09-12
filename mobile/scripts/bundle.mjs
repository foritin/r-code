/**
 * R21.A2 — RN bundle 门禁（离线可执行，不依赖 @react-native/cli）：
 * 直接用 metro 的打包 API 为 android/ios 产出 JS bundle。
 *
 * 这是 RN 侧**唯一能被 CI 真正跑起来**的构建验证——本机 Windows 无法产
 * APK/IPA（需要 Android SDK / Xcode），但 bundle 能证明 App.tsx 及其依赖
 * （含 RN 设置屏与共享 core）在 RN 工具链下可被解析与打包。
 *
 * 用法：node scripts/bundle.mjs <android|ios>
 */
import { existsSync, mkdirSync, rmSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(here, "..");

const platform = process.argv[2];
if (!["android", "ios"].includes(platform)) {
  console.error("usage: node scripts/bundle.mjs <android|ios>");
  process.exit(64);
}

const { getDefaultConfig, mergeConfig } = require("@react-native/metro-config");
const Metro = require("metro");

// 共享 core 在 mobile/ 之外（R21：PWA 与 RN 共用），必须显式加入
// watchFolders；getDefaultConfig 返回的 watchFolders 是空数组，不加会让
// metro 报 "Failed to get the SHA-1 ... file is not watched"。
const sharedCore = resolve(projectRoot, "../src-tauri/frontend/src/remote");
const config = mergeConfig(getDefaultConfig(projectRoot), {
  projectRoot,
  watchFolders: [projectRoot, sharedCore],
  resolver: {
    sourceExts: ["tsx", "ts", "jsx", "js", "json"],
    // 共享 core 在 mobile/ 之外，babel helper（@babel/runtime 等）必须
    // 回落到 mobile 的 node_modules，否则解析失败。
    nodeModulesPaths: [join(projectRoot, "node_modules")],
  },
});
const outDir = join(projectRoot, "bundle-out");
const bundleOutput = join(outDir, `main.${platform}.jsbundle`);
const assetsDest = join(outDir, platform);

rmSync(bundleOutput, { force: true });
mkdirSync(outDir, { recursive: true });

await Metro.runBuild(config, {
  entry: "App.tsx",
  platform,
  minify: false,
  dev: false,
  out: bundleOutput,
  assetsDest,
  sourceMap: false,
});

// metro 可能给 out 追加后缀（.jsbundle → .jsbundle.js）。
const produced = [bundleOutput, `${bundleOutput}.js`].find(
  (candidate) => existsSync(candidate) && statSync(candidate).size > 0,
);
if (!produced) {
  console.error(`bundle failed: ${bundleOutput} is missing or empty`);
  process.exit(1);
}

const bytes = statSync(produced).size;
// RN bundle 必然包含 RN runtime 与共享 core 的产物；过小的产物说明
// 入口没被打进去（例如 import 路径错误导致空 bundle）。
if (bytes < 50_000) {
  console.error(`bundle suspiciously small: ${bytes} bytes`);
  process.exit(1);
}

console.log(`bundle ok: ${produced} (${bytes} bytes)`);
