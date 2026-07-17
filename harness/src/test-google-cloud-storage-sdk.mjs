import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import net from "node:net";
import { setTimeout as sleep } from "node:timers/promises";
import { Storage } from "@google-cloud/storage";

const repoRoot = new URL("../../", import.meta.url);
const port = await getFreePort();
const baseUrl = `http://127.0.0.1:${port}`;
const previousStorageEmulatorHost = process.env.STORAGE_EMULATOR_HOST;
process.env.STORAGE_EMULATOR_HOST = baseUrl;

const daemon = spawn(
  "cargo",
  ["run", "-p", "firelite", "--", "daemon", "--host", "127.0.0.1", "--port", String(port)],
  {
    cwd: repoRoot,
    stdio: ["ignore", "pipe", "pipe"]
  }
);

let output = "";
daemon.stdout.on("data", (chunk) => {
  output += chunk.toString();
});
daemon.stderr.on("data", (chunk) => {
  output += chunk.toString();
});

try {
  await waitForHealth(baseUrl, daemon);

  const storage = new Storage({
    projectId: "demo-firelite",
    apiEndpoint: baseUrl
  });
  const bucket = storage.bucket("demo-firelite.appspot.com");
  const objectPath = `google-cloud-storage/${Date.now()}/statement.csv`;
  const file = bucket.file(objectPath);
  const contents = Buffer.from("date,amount\n2026-07-06,12.34\n", "utf8");

  await file.save(contents, {
    resumable: true,
    contentType: "text/csv",
    metadata: {
      metadata: {
        "user-id": "gcs-user"
      }
    }
  });

  const [metadata] = await file.getMetadata();
  assert.equal(metadata.bucket, "demo-firelite.appspot.com");
  assert.equal(metadata.name, objectPath);
  assert.equal(Number(metadata.size), contents.length);
  assert.equal(metadata.contentType, "text/csv");
  assert.equal(metadata.metadata["user-id"], "gcs-user");

  const [downloaded] = await file.download();
  assert.equal(downloaded.toString("utf8"), contents.toString("utf8"));

  await file.delete();
  await assert.rejects(() => file.getMetadata());

  console.log("@google-cloud/storage resumable SDK E2E passed");
} finally {
  daemon.kill("SIGTERM");
  if (previousStorageEmulatorHost === undefined) {
    delete process.env.STORAGE_EMULATOR_HOST;
  } else {
    process.env.STORAGE_EMULATOR_HOST = previousStorageEmulatorHost;
  }
}

async function waitForHealth(baseUrl, child) {
  const startedAt = Date.now();
  while (Date.now() - startedAt < 15_000) {
    if (child.exitCode !== null) {
      throw new Error(`firelite daemon exited early with ${child.exitCode}\n${output}`);
    }

    try {
      const response = await fetch(`${baseUrl}/__/health`);
      if (response.ok) {
        return;
      }
    } catch {
      // Daemon is still compiling or binding.
    }

    await sleep(100);
  }

  throw new Error(`timed out waiting for firelite daemon\n${output}`);
}

async function getFreePort() {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}
