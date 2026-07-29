import assert from "node:assert/strict";
import http from "node:http";
import test from "node:test";

import { verifyLoopback } from "./check-loopback.mjs";
import { classifyDevServer, runDevServer } from "./tauri-dev-server.mjs";

async function withServer(body, callback) {
  const server = http.createServer((_request, response) => {
    response.writeHead(200, { "content-type": "text/html" });
    response.end(body);
  });

  await new Promise((resolveListen, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolveListen);
  });

  const address = server.address();
  assert.ok(address && typeof address !== "string");
  try {
    await callback(address.port);
  } finally {
    await new Promise((resolveClose) => server.close(resolveClose));
  }
}

async function occupyPortIfFree(port) {
  const server = http.createServer((_request, response) => response.end("occupied"));
  return new Promise((resolveListen, reject) => {
    server.once("error", (error) => {
      if (error && error.code === "EADDRINUSE") resolveListen(null);
      else reject(error);
    });
    server.listen(port, "127.0.0.1", () => resolveListen(server));
  });
}

test("loopback validation uses an ephemeral port", async () => {
  const blocker = await occupyPortIfFree(5173);
  try {
    const port = await verifyLoopback();
    assert.ok(Number.isInteger(port) && port > 0);
    assert.notEqual(port, 5173);
  } finally {
    if (blocker) {
      await new Promise((resolveClose) => blocker.close(resolveClose));
    }
  }
});

test("recognizes and reuses an existing R-Code Vite server", async () => {
  await withServer(
    '<!doctype html><html><head><title>R-Code</title><script type="module" src="/@vite/client"></script></head></html>',
    async (port) => {
      assert.equal(await classifyDevServer({ port }), "r-code");
      assert.equal(await runDevServer({ port }), "reused");
    },
  );
});

test("rejects a foreign service instead of reporting a loopback failure", async () => {
  await withServer("<!doctype html><title>Another application</title>", async (port) => {
    assert.equal(await classifyDevServer({ port }), "occupied");
    await assert.rejects(() => runDevServer({ port }), /already in use by another service/);
  });
});

test("reports an unused port as free", async () => {
  const holder = http.createServer();
  await new Promise((resolveListen, reject) => {
    holder.once("error", reject);
    holder.listen(0, "127.0.0.1", resolveListen);
  });
  const address = holder.address();
  assert.ok(address && typeof address !== "string");
  const port = address.port;
  await new Promise((resolveClose) => holder.close(resolveClose));

  assert.equal(await classifyDevServer({ port }), "free");
});
