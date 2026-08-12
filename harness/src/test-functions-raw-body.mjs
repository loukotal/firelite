import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";

const harnessDir = fileURLToPath(new URL("../", import.meta.url));
const repoRoot = fileURLToPath(new URL("../../", import.meta.url));
const functionsDir = await mkdtemp(path.join(os.tmpdir(), "firelite-raw-body-"));
const port = await getFreePort();
const baseUrl = `http://127.0.0.1:${port}`;
const endpoint = `${baseUrl}/demo-firelite/us-central1/api`;

await symlink(
  path.join(harnessDir, "node_modules"),
  path.join(functionsDir, "node_modules"),
  process.platform === "win32" ? "junction" : "dir"
);
await writeFunctionSource("initial");

const functions = spawn(
  "cargo",
  [
    "run",
    "-p",
    "firelite",
    "--",
    "functions",
    "--project",
    "demo-firelite",
    "--host",
    "127.0.0.1",
    "--port",
    String(port),
    "--watch",
    functionsDir
  ],
  {
    cwd: repoRoot,
    stdio: ["ignore", "pipe", "pipe"]
  }
);

let output = "";
functions.stdout.on("data", (chunk) => {
  output += chunk.toString();
});
functions.stderr.on("data", (chunk) => {
  output += chunk.toString();
});

try {
  await waitForHealth();

  const jsonPayload = '{\n  "message": "preserve spacing",\n  "count": 2\n}\n';
  const json = await invoke(jsonPayload, "application/json");
  assertRawBody(json, Buffer.from(jsonPayload));
  assert.deepEqual(json.body, { message: "preserve spacing", count: 2 });

  const textPayload = "line one\r\nline two\n";
  const text = await invoke(textPayload, "text/plain");
  assertRawBody(text, Buffer.from(textPayload));
  assert.equal(text.body, textPayload);

  const empty = await invoke();
  assertRawBody(empty, Buffer.alloc(0));

  const binaryPayload = Buffer.from([0, 255, 128, 10, 123, 0]);
  const binary = await invoke(binaryPayload, "application/octet-stream");
  assertRawBody(binary, binaryPayload);
  assert.equal(binary.bodyIsBuffer, true);

  await writeFunctionSource("reloaded");
  await waitForSourceVersion("reloaded");

  const afterReload = await invoke(jsonPayload, "application/json");
  assert.equal(afterReload.sourceVersion, "reloaded");
  assertRawBody(afterReload, Buffer.from(jsonPayload));
  assert.deepEqual(afterReload.body, { message: "preserve spacing", count: 2 });

  console.log("functions rawBody Express E2E passed");
} finally {
  functions.kill("SIGTERM");
  await rm(functionsDir, { recursive: true, force: true });
}

async function invoke(body, contentType) {
  const headers = contentType ? { "content-type": contentType } : undefined;
  const response = await fetch(endpoint, { method: "POST", headers, body });
  const text = await response.text();
  assert.equal(response.ok, true, text || output);
  return JSON.parse(text);
}

function assertRawBody(response, expected) {
  assert.equal(response.rawBodyIsBuffer, true);
  assert.deepEqual(response.rawBodyBytes, Array.from(expected));
}

async function waitForHealth() {
  const startedAt = Date.now();
  while (Date.now() - startedAt < 20_000) {
    if (functions.exitCode !== null) {
      throw new Error(`firelite functions exited early with ${functions.exitCode}\n${output}`);
    }

    try {
      const response = await fetch(`${baseUrl}/__/health`);
      if (response.ok) {
        return;
      }
    } catch {
      // Firelite may still be compiling or binding.
    }

    await sleep(100);
  }

  throw new Error(`timed out waiting for firelite functions\n${output}`);
}

async function waitForSourceVersion(expected) {
  const startedAt = Date.now();
  while (Date.now() - startedAt < 10_000) {
    try {
      const response = await invoke(Buffer.from("reload"), "application/octet-stream");
      if (response.sourceVersion === expected) {
        return;
      }
    } catch {
      // The old worker can briefly be unavailable while the replacement starts.
    }

    await sleep(100);
  }

  throw new Error(`timed out waiting for functions worker reload\n${output}`);
}

async function writeFunctionSource(sourceVersion) {
  await writeFile(
    path.join(functionsDir, "index.js"),
    `
const express = require("express");

const app = express();
app.use(express.json());
app.use(express.text());
app.use(express.raw({ type: "*/*" }));
app.all("*", (req, res) => {
  res.json({
    sourceVersion: ${JSON.stringify(sourceVersion)},
    rawBodyIsBuffer: Buffer.isBuffer(req.rawBody),
    rawBodyBytes: Array.from(req.rawBody),
    body: req.body ?? null,
    bodyIsBuffer: Buffer.isBuffer(req.body)
  });
});

exports.api = app;
exports.api.__trigger = {
  name: "api",
  regions: ["us-central1"],
  httpsTrigger: {}
};
`
  );
}

async function getFreePort() {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port: availablePort } = server.address();
      server.close(() => resolve(availablePort));
    });
  });
}
