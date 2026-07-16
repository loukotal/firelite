import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import net from "node:net";
import { setTimeout as sleep } from "node:timers/promises";
import { initializeApp, deleteApp } from "firebase/app";
import {
  connectStorageEmulator,
  deleteObject,
  getBytes,
  getMetadata,
  getStorage,
  listAll,
  ref,
  uploadBytes,
  uploadBytesResumable
} from "firebase/storage";

const repoRoot = new URL("../../", import.meta.url);
const port = await getFreePort();
const baseUrl = `http://127.0.0.1:${port}`;
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

  const app = initializeApp({
    apiKey: "fake",
    authDomain: "demo-firelite.firebaseapp.com",
    projectId: "demo-firelite",
    storageBucket: "demo-firelite.appspot.com",
    appId: "1:123:web:firelite"
  });
  const storage = getStorage(app);
  connectStorageEmulator(storage, "127.0.0.1", port);

  const objectPath = `sdk/${Date.now()}/hello.txt`;
  const objectRef = ref(storage, objectPath);
  const bytes = new TextEncoder().encode("hello from firebase/storage");

  const uploaded = await uploadBytes(objectRef, bytes, {
    contentType: "text/plain"
  });
  assert.equal(uploaded.metadata.bucket, "demo-firelite.appspot.com");
  assert.equal(uploaded.metadata.fullPath, objectPath);
  assert.equal(uploaded.metadata.size, bytes.length);
  assert.equal(uploaded.metadata.contentType, "text/plain");

  const metadata = await getMetadata(objectRef);
  assert.equal(metadata.fullPath, objectPath);
  assert.equal(metadata.size, bytes.length);

  const downloaded = await getBytes(objectRef);
  assert.equal(new TextDecoder().decode(downloaded), "hello from firebase/storage");

  const listed = await listAll(ref(storage, objectPath.split("/").slice(0, -1).join("/")));
  assert.equal(listed.items.length, 1);
  assert.equal(listed.items[0].fullPath, objectPath);

  await deleteObject(objectRef);
  await assert.rejects(() => getMetadata(objectRef));

  const resumableRef = ref(storage, `sdk/${Date.now()}/lease.pdf`);
  const resumableBytes = new Uint8Array(300_000).fill(42);
  const resumable = await uploadBytesResumable(resumableRef, resumableBytes, {
    contentType: "application/pdf"
  });
  assert.equal(resumable.metadata.size, resumableBytes.length);

  await deleteApp(app);
  console.log("firebase/storage SDK E2E passed");
} finally {
  daemon.kill("SIGTERM");
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
