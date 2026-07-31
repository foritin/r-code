import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import vm from "node:vm";

const repoRoot = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/(?:[A-Za-z]:)/, (value) => value.slice(1))), "..");
const frontend = path.join(repoRoot, "installer", "frontend");
const appPath = path.join(frontend, "app.js");
const watchdogPath = path.join(frontend, "watchdog.js");
const indexPath = path.join(frontend, "index.html");
const iconPath = path.join(frontend, "icon.png");
const capabilityPath = path.join(repoRoot, "installer", "capabilities", "default.json");

const appSource = fs.readFileSync(appPath, "utf8");
const watchdogSource = fs.readFileSync(watchdogPath, "utf8");
const indexSource = fs.readFileSync(indexPath, "utf8");
const capability = JSON.parse(fs.readFileSync(capabilityPath, "utf8"));

// Tauri's runtime bootstrap owns global lexical names. Compiling the application
// beside a representative injected binding catches the collision that previously
// made every installer control inert in the packaged WebView2 window.
new vm.Script(`const isTauri = true;\n${appSource}`, { filename: appPath });
new vm.Script(`const isTauri = true;\n${watchdogSource}`, { filename: watchdogPath });

if (!indexSource.includes('<script src="app.js" defer></script>')) {
  throw new Error("installer index must load app.js as a deferred external script");
}
if (!indexSource.includes('<script src="watchdog.js" defer></script>')) {
  throw new Error("installer index must load the independent boot watchdog");
}
if (!/<button[^>]+id="install-now"[^>]+disabled/.test(indexSource)) {
  throw new Error("install action must remain disabled until runtime initialization completes");
}
for (const permission of ["core:event:allow-listen", "core:event:allow-unlisten"]) {
  if (!capability.permissions?.includes(permission)) {
    throw new Error(`installer capability is missing ${permission}`);
  }
}

const icon = fs.readFileSync(iconPath);
if (icon.length < 24 || icon.toString("ascii", 1, 4) !== "PNG") {
  throw new Error("installer frontend icon is not a valid PNG");
}
const width = icon.readUInt32BE(16);
const height = icon.readUInt32BE(20);
if (width < 512 || height < 512) {
  throw new Error(`installer frontend icon must be at least 512x512, got ${width}x${height}`);
}

console.log(`installer frontend preflight passed (${width}x${height} icon)`);
