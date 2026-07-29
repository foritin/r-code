import http from "node:http";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const host = "127.0.0.1";
const timeoutMs = 2_000;

function listen(server, listenHost) {
  return new Promise((resolveListen, reject) => {
    server.once("error", reject);
    server.listen(0, listenHost, resolveListen);
  });
}

function request(requestHost, requestPort, requestTimeoutMs) {
  return new Promise((resolveRequest, reject) => {
    const client = http.get(
      { host: requestHost, port: requestPort, path: "/", timeout: requestTimeoutMs },
      (response) => {
        response.resume();
        response.once("end", () => {
          if (response.statusCode === 200) resolveRequest();
          else reject(new Error(`unexpected status ${response.statusCode}`));
        });
      },
    );

    client.once("timeout", () => client.destroy(new Error("connection timed out")));
    client.once("error", reject);
  });
}

export async function verifyLoopback({ listenHost = host, requestTimeoutMs = timeoutMs } = {}) {
  const server = http.createServer((_request, response) => {
    response.writeHead(200, { "content-type": "text/plain" });
    response.end("ok");
  });

  try {
    await listen(server, listenHost);
    const address = server.address();
    if (!address || typeof address === "string") {
      throw new Error("could not determine the loopback test port");
    }
    await request(listenHost, address.port, requestTimeoutMs);
    return address.port;
  } finally {
    if (server.listening) {
      await new Promise((resolveClose) => server.close(resolveClose));
    }
  }
}

function isEntrypoint() {
  return Boolean(process.argv[1]) && pathToFileURL(resolve(process.argv[1])).href === import.meta.url;
}

if (isEntrypoint()) {
  try {
    await verifyLoopback();
  } catch (error) {
    console.error(`[R-Code] Local TCP loopback (${host}, ephemeral port) is unavailable.`);
    if (process.platform === "win32") {
      console.error(
        "[R-Code] Disable VPN/TUN interception, or exclude 127.0.0.0/8 from it. " +
          "For Clash Verge, turn off TUN mode or configure route-exclude-address.",
      );
    }
    console.error(`[R-Code] ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}
