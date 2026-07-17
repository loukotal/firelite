import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import { spawn } from "node:child_process";
import net from "node:net";
import { setTimeout as sleep } from "node:timers/promises";
import { initializeApp, deleteApp } from "firebase/app";
import {
  connectAuthEmulator,
  createUserWithEmailAndPassword,
  getAuth,
  isSignInWithEmailLink,
  getMultiFactorResolver,
  sendSignInLinkToEmail,
  signInWithCustomToken,
  signInAnonymously,
  signInWithEmailAndPassword,
  signInWithEmailLink,
  signOut
} from "firebase/auth";

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
    appId: "1:123:web:firelite"
  });
  const auth = getAuth(app);
  connectAuthEmulator(auth, baseUrl, { disableWarnings: true });

  await testPasswordFlow(auth);
  await testAnonymousFlow(auth);
  await testPhoneMfaFlow(auth, baseUrl);
  await testCustomTokenFlow(auth);
  await testEmailLinkFlow(auth, baseUrl);

  await deleteApp(app);
  console.log("firebase/auth SDK E2E passed");
} finally {
  daemon.kill("SIGTERM");
}

async function testAnonymousFlow(auth) {
  await signOut(auth);
  const credential = await signInAnonymously(auth);
  assert.equal(credential.user.isAnonymous, true);
  assert.equal(credential.user.email, null);
  assert.equal(credential.user.providerData.length, 0);
  const tokenResult = await credential.user.getIdTokenResult();
  assert.equal(tokenResult.signInProvider, "anonymous");
  await credential.user.reload();
  const repeated = await signInAnonymously(auth);
  assert.equal(repeated.user.uid, credential.user.uid);
}

async function testPhoneMfaFlow(auth, baseUrl) {
  await signOut(auth);
  const email = `sdk-mfa-${Date.now()}@example.test`;
  const password = "secret123";
  const created = await createUserWithEmailAndPassword(auth, email, password);
  const idToken = await created.user.getIdToken();
  const enrollment = await postJson(
    `${baseUrl}/identitytoolkit.googleapis.com/v2/accounts/mfaEnrollment:start?key=fake`,
    {
      idToken,
      phoneEnrollmentInfo: {
        phoneNumber: "+15555550123",
        clientType: "CLIENT_TYPE_WEB"
      }
    }
  );
  const enrollmentCodes = await fetchJson(
    `${baseUrl}/emulator/v1/projects/demo-firelite/verificationCodes`
  );
  await postJson(
    `${baseUrl}/identitytoolkit.googleapis.com/v2/accounts/mfaEnrollment:finalize?key=fake`,
    {
      idToken,
      displayName: "SDK phone",
      phoneVerificationInfo: {
        sessionInfo: enrollment.phoneSessionInfo.sessionInfo,
        code: enrollmentCodes.verificationCodes[0].code
      }
    }
  );

  await signOut(auth);
  let mfaError;
  try {
    await signInWithEmailAndPassword(auth, email, password);
    assert.fail("MFA user signed in without a second factor");
  } catch (error) {
    assert.equal(error.code, "auth/multi-factor-auth-required");
    mfaError = error;
  }

  const resolver = getMultiFactorResolver(auth, mfaError);
  assert.equal(resolver.hints.length, 1);
  assert.equal(resolver.hints[0].factorId, "phone");
  assert.equal(resolver.hints[0].phoneNumber, "+15555550123");
  assert.equal(resolver.hints[0].displayName, "SDK phone");

  const firstFactor = await postJson(
    `${baseUrl}/identitytoolkit.googleapis.com/v1/accounts:signInWithPassword?key=fake`,
    { email, password, returnSecureToken: true }
  );
  const signIn = await postJson(
    `${baseUrl}/identitytoolkit.googleapis.com/v2/accounts/mfaSignIn:start?key=fake`,
    {
      mfaPendingCredential: firstFactor.mfaPendingCredential,
      mfaEnrollmentId: resolver.hints[0].uid,
      phoneSignInInfo: {}
    }
  );
  const verificationId = signIn.phoneResponseInfo.sessionInfo;
  const signInCodes = await fetchJson(
    `${baseUrl}/emulator/v1/projects/demo-firelite/verificationCodes`
  );
  const code = signInCodes.verificationCodes.find(
    (entry) => entry.sessionInfo === verificationId
  ).code;
  const signedIn = await postJson(
    `${baseUrl}/identitytoolkit.googleapis.com/v2/accounts/mfaSignIn:finalize?key=fake`,
    {
      mfaPendingCredential: firstFactor.mfaPendingCredential,
      phoneVerificationInfo: { sessionInfo: verificationId, code }
    }
  );
  assert.ok(signedIn.idToken);
  assert.ok(signedIn.refreshToken);
}

async function testPasswordFlow(auth) {
  const email = `sdk-password-${Date.now()}@example.test`;
  const password = "secret123";

  const created = await createUserWithEmailAndPassword(auth, email, password);
  assert.equal(created.user.email, email);
  assert.ok(created.user.uid);

  await signOut(auth);

  const signedIn = await signInWithEmailAndPassword(auth, email, password);
  assert.equal(signedIn.user.uid, created.user.uid);
  assert.equal(signedIn.user.email, email);

  const tokenResult = await signedIn.user.getIdTokenResult();
  assert.equal(tokenResult.signInProvider, "password");
  assert.equal(tokenResult.claims.email, email);
  assert.equal(tokenResult.claims.email_verified, false);
  assert.equal(tokenResult.claims.firebase.sign_in_provider, "password");
  assert.ok(tokenResult.claims.auth_time);
}

async function testCustomTokenFlow(auth) {
  const credential = await signInWithCustomToken(
    auth,
    unsignedJwt({ uid: "sdk-custom-token-user" })
  );
  assert.equal(credential.user.uid, "sdk-custom-token-user");
  assert.equal(credential.user.email, "sdk-custom-token-user@custom-token.local");
}

async function testEmailLinkFlow(auth, baseUrl) {
  const email = `sdk-link-${Date.now()}@example.test`;
  const continueUrl = "http://localhost/finish-email-link";
  await sendSignInLinkToEmail(auth, email, {
    url: continueUrl,
    handleCodeInApp: true
  });

  const oobCodes = await fetchJson(`${baseUrl}/emulator/v1/projects/demo-firelite/oobCodes`);
  assert.equal(oobCodes.oobCodes.length, 1);
  const code = oobCodes.oobCodes[0].oobCode;
  const link = `${continueUrl}?mode=signIn&oobCode=${encodeURIComponent(code)}&apiKey=fake`;

  assert.equal(isSignInWithEmailLink(auth, link), true);

  const credential = await signInWithEmailLink(auth, email, link);
  assert.equal(credential.user.email, email);
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

async function fetchJson(url) {
  const response = await fetch(url);
  assert.equal(response.ok, true, `${url} returned ${response.status}`);
  return await response.json();
}

async function postJson(url, body) {
  const response = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body)
  });
  if (!response.ok) {
    throw new Error(`${url} returned ${response.status}: ${await response.text()}`);
  }
  return await response.json();
}

function unsignedJwt(payload) {
  const header = base64Url(JSON.stringify({ alg: "none", typ: "JWT" }));
  const body = base64Url(JSON.stringify(payload));
  return `${header}.${body}.`;
}

function base64Url(value) {
  return Buffer.from(value)
    .toString("base64")
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replaceAll("=", "");
}
