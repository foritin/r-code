import http from "node:http";

const host = "127.0.0.1";
const port = 5173;
const timeoutMs = 2_000;
const server = http.createServer((_request, response) => {
  response.writeHead(200, { "content-type": "text/plain" });
  response.end("ok");
});

function listen() {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, host, resolve);
  });
}

function close() {
  return new Promise((resolve) => server.close(resolve));
}

function request(port) {
  return new Promise((resolve, reject) => {
    const client = http.get({ host, port, path: "/", timeout: timeoutMs }, (response) => {
      response.resume();
      response.once("end", () => {
        if (response.statusCode === 200) resolve();
        else reject(new Error(`unexpected status ${response.statusCode}`));
      });
    });

    client.once("timeout", () => client.destroy(new Error("connection timed out")));
    client.once("error", reject);
  });
}

try {
  await listen();
  await request(port);
} catch (error) {
  console.error(`[R-Code] Local TCP loopback (${host}:${port}) is unavailable.`);
  if (process.platform === "win32") {
    console.error(
      "[R-Code] Disable VPN/TUN interception, or exclude 127.0.0.0/8 from it. " +
        "For Clash Verge, turn off TUN mode or configure route-exclude-address.",
    );
  }
  console.error(`[R-Code] ${error instanceof Error ? error.message : String(error)}`);
  process.exitCode = 1;
} finally {
  if (server.listening) await close();
}
