import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import net from "node:net";
import { setTimeout as sleep } from "node:timers/promises";
import { PubSub } from "@google-cloud/pubsub";

const repoRoot = new URL("../../", import.meta.url);
const port = await getFreePort();
const baseUrl = `http://127.0.0.1:${port}`;
const previousPubsubEmulatorHost = process.env.PUBSUB_EMULATOR_HOST;
process.env.PUBSUB_EMULATOR_HOST = `127.0.0.1:${port}`;
process.env.GCLOUD_PROJECT = "demo-firelite";
process.env.GOOGLE_CLOUD_PROJECT = "demo-firelite";

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

  const pubsub = new PubSub({ projectId: "demo-firelite" });
  const topic = pubsub.topic(`sdk-topic-${Date.now()}`);
  const [createdTopic] = await withTimeout(topic.create(), "topic.create");
  assert.ok(createdTopic.name.includes("/topics/"));

  const subscription = topic.subscription(`sdk-sub-${Date.now()}`);
  await withTimeout(subscription.create(), "subscription.create");

  const messageId = await withTimeout(
    topic.publishMessage({
      data: Buffer.from("hello from node sdk"),
      attributes: { source: "node-sdk" }
    }),
    "topic.publishMessage"
  );
  assert.ok(messageId);

  const response = await withTimeout(
    request(subscription, {
      client: "SubscriberClient",
      method: "pull",
      reqOpts: {
        subscription: subscription.name,
        maxMessages: 1
      },
      gaxOpts: {}
    }),
    "subscription.pull"
  );
  assert.equal(response.receivedMessages.length, 1);
  const received = response.receivedMessages[0];
  assert.equal(received.message.data.toString(), "hello from node sdk");
  assert.equal(received.message.attributes.source, "node-sdk");

  console.log("google-cloud/pubsub SDK E2E passed");
} finally {
  daemon.kill("SIGTERM");
  if (previousPubsubEmulatorHost === undefined) {
    delete process.env.PUBSUB_EMULATOR_HOST;
  } else {
    process.env.PUBSUB_EMULATOR_HOST = previousPubsubEmulatorHost;
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

async function withTimeout(promise, label) {
  let timeout;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timeout = setTimeout(() => {
          reject(new Error(`${label} timed out\n${output}`));
        }, 10_000);
      })
    ]);
  } finally {
    clearTimeout(timeout);
  }
}

async function request(target, options) {
  return await new Promise((resolve, reject) => {
    target.request(options, (error, response) => {
      if (error) {
        reject(error);
      } else {
        resolve(response);
      }
    });
  });
}
