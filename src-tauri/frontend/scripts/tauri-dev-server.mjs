import { spawn } from "node:child_process";
import net from "node:net";

const port = 1420;
const devUrl = `http://localhost:${port}/`;
const probeUrls = [devUrl, `http://127.0.0.1:${port}/`, `http://[::1]:${port}/`];
const rCodeMarker = '<title>R-Code</title>';

function portIsOpenAt(host) {
  return new Promise((resolve) => {
    const socket = net.createConnection({ host, port });
    const finish = (value) => {
      socket.removeAllListeners();
      socket.destroy();
      resolve(value);
    };
    socket.once("connect", () => finish(true));
    socket.once("error", () => finish(false));
    socket.setTimeout(700, () => finish(false));
  });
}

async function portIsOpen() {
  const results = await Promise.all(["127.0.0.1", "::1"].map(portIsOpenAt));
  return results.some(Boolean);
}

async function hasRCodeViteServer() {
  for (const url of probeUrls) {
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(1_000) });
      const page = await response.text();
      if (response.ok && page.includes(rCodeMarker) && page.includes("/@vite/client")) {
        return true;
      }
    } catch {
      // Probe the next loopback family before treating the port as unused.
    }
  }
  return false;
}

async function waitForRCodeViteServer() {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    if (await hasRCodeViteServer()) return true;
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  return false;
}

if (await hasRCodeViteServer()) {
  console.log(`Reusing the R-Code Vite server at ${devUrl}`);
  process.exit(0);
}

if (await portIsOpen()) {
  console.error(
    `Port ${port} is already in use by a server other than R-Code. Stop that process or change the dev port before starting Tauri.`,
  );
  process.exit(1);
}

const vite = spawn(process.execPath, ["./node_modules/vite/bin/vite.js"], {
  cwd: process.cwd(),
  detached: true,
  stdio: "ignore",
  windowsHide: true,
});
vite.unref();

if (await waitForRCodeViteServer()) {
  console.log(`Started the R-Code Vite server at ${devUrl}`);
  process.exit(0);
}

console.error(`R-Code Vite did not become ready at ${devUrl}. Run \`npm run dev\` in src-tauri/frontend for details.`);
process.exit(1);
