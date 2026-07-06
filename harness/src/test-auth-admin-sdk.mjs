import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import net from "node:net";
import { setTimeout as sleep } from "node:timers/promises";
import { initializeApp, deleteApp } from "firebase-admin/app";
import { getAuth } from "firebase-admin/auth";

const repoRoot = new URL("../../", import.meta.url);
const port = await getFreePort();
const baseUrl = `http://127.0.0.1:${port}`;
const previousAuthEmulatorHost = process.env.FIREBASE_AUTH_EMULATOR_HOST;
process.env.FIREBASE_AUTH_EMULATOR_HOST = `127.0.0.1:${port}`;
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

  const app = initializeApp({ projectId: "demo-firelite" }, "admin-sdk-e2e");
  const auth = getAuth(app);
  const uid = `admin-sdk-${Date.now()}`;
  const email = `${uid}@example.test`;

  const created = await auth.createUser({
    uid,
    email,
    password: "secret123",
    displayName: "Admin SDK User"
  });
  assert.equal(created.uid, uid);
  assert.equal(created.email, email);
  assert.equal(created.displayName, "Admin SDK User");

  const byUid = await auth.getUser(uid);
  assert.equal(byUid.uid, uid);
  assert.equal(byUid.email, email);

  const byEmail = await auth.getUserByEmail(email);
  assert.equal(byEmail.uid, uid);
  assert.equal(byEmail.email, email);

  const phoneNumber = "+15555550123";
  const updated = await auth.updateUser(uid, {
    phoneNumber,
    disabled: true,
    multiFactor: {
      enrolledFactors: [
        {
          uid: "factor-admin-sdk",
          factorId: "phone",
          phoneNumber: "+15555550124",
          displayName: "Admin SDK phone",
          enrollmentTime: "Tue, 01 Jan 2030 00:00:00 GMT"
        }
      ]
    }
  });
  assert.equal(updated.phoneNumber, phoneNumber);
  assert.equal(updated.disabled, true);
  assert.equal(updated.multiFactor.enrolledFactors.length, 1);
  assert.equal(updated.multiFactor.enrolledFactors[0].uid, "factor-admin-sdk");
  assert.equal(updated.multiFactor.enrolledFactors[0].phoneNumber, "+15555550124");

  const byPhone = await auth.getUserByPhoneNumber(phoneNumber);
  assert.equal(byPhone.uid, uid);
  assert.equal(byPhone.multiFactor.enrolledFactors[0].uid, "factor-admin-sdk");

  await assert.rejects(
    () =>
      auth.createUser({
        uid: `${uid}-dupe-phone`,
        email: `${uid}-dupe-phone@example.test`,
        phoneNumber
      }),
    (error) => error.code === "auth/phone-number-already-exists"
  );

  await auth.updateUser(uid, { disabled: false });
  const signIn = await fetch(
    `${baseUrl}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        email,
        password: "secret123",
        returnSecureToken: true
      })
    }
  );
  assert.equal(signIn.ok, true);
  const signInBody = await signIn.json();
  await sleep(1100);
  await auth.revokeRefreshTokens(uid);
  const revoked = await auth.getUser(uid);
  assert.ok(revoked.tokensValidAfterTime);
  await assert.rejects(
    () => auth.verifyIdToken(signInBody.idToken, true),
    (error) => error.code === "auth/id-token-revoked"
  );

  const resetLink = await auth.generatePasswordResetLink(email);
  assert.match(resetLink, /mode=resetPassword/);
  assert.match(resetLink, /oobCode=firelite-oob-/);

  const listed = await auth.listUsers(100);
  assert.ok(listed.users.some((user) => user.uid === uid));

  await auth.deleteUser(uid);

  await assert.rejects(
    () => auth.getUser(uid),
    (error) => error.code === "auth/user-not-found"
  );

  await deleteApp(app);
  console.log("firebase-admin/auth SDK E2E passed");
} finally {
  daemon.kill("SIGTERM");
  if (previousAuthEmulatorHost === undefined) {
    delete process.env.FIREBASE_AUTH_EMULATOR_HOST;
  } else {
    process.env.FIREBASE_AUTH_EMULATOR_HOST = previousAuthEmulatorHost;
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
