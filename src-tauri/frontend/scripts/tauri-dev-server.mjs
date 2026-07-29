import net from "node:net";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const defaultHost = "127.0.0.1";
const defaultPort = 5173;
const requestTimeoutMs = 1_000;
const rCodeTitle = "<title>R-Code</title>";
const viteClientMarker = "/@vite/client";
const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const frontendDirectory = resolve(scriptDirectory, "..");

function portIsOpen(host, port, timeoutMs = 700) {
  return new Promise((resolveOpen) => {
    const socket = net.createConnection({ host, port });
    let settled = false;
    const finish = (open) => {
      if (settled) return;
      settled = true;
      socket.removeAllListeners();
      socket.destroy();
      resolveOpen(open);
    };

    socket.once("connect", () => finish(true));
    socket.once("error", () => finish(false));
    socket.setTimeout(timeoutMs, () => finish(false));
  });
}

async function readHttpPage(url, timeoutMs) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch(url, { signal: controller.signal });
    return { ok: response.ok, page: await response.text() };
  } finally {
    clearTimeout(timer);
  }
}

export async function classifyDevServer({
  host = defaultHost,
  port = defaultPort,
  timeoutMs = requestTimeoutMs,
} = {}) {
  const url = `http://${host}:${port}/`;
  try {
    const response = await readHttpPage(url, timeoutMs);
    if (response.ok && response.page.includes(rCodeTitle) && response.page.includes(viteClientMarker)) {
      return "r-code";
    }
    return "occupied";
  } catch {
    return (await portIsOpen(host, port)) ? "occupied" : "free";
  }
}

async function startVite(host, port) {
  const { createServer } = await import("vite");
  const server = await createServer({
    configFile: resolve(frontendDirectory, "vite.config.ts"),
    root: frontendDirectory,
    server: { host, port, strictPort: true },
  });

  await server.listen();
  server.printUrls();

  let closing = false;
  const close = async () => {
    if (closing) return;
    closing = true;
    await server.close();
  };

  process.once("SIGINT", () => void close());
  process.once("SIGTERM", () => void close());
}

export async function runDevServer({ host = defaultHost, port = defaultPort } = {}) {
  const url = `http://${host}:${port}/`;
  const state = await classifyDevServer({ host, port });

  if (state === "r-code") {
    console.log(`[R-Code] Reusing the existing Vite server at ${url}`);
    return "reused";
  }

  if (state === "occupied") {
    throw new Error(
      `[R-Code] ${host}:${port} is already in use by another service. ` +
        "Stop that service or change both the Vite port and Tauri devUrl.",
    );
  }

  await startVite(host, port);
  return "started";
}

function isEntrypoint() {
  return Boolean(process.argv[1]) && pathToFileURL(resolve(process.argv[1])).href === import.meta.url;
}

if (isEntrypoint()) {
  try {
    await runDevServer();
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}
