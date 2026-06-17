import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";

const fixturePath = resolve("fixtures/official/auth-basic-password-flow.json");

const envSnapshot = Object.fromEntries(
  [
    "FIREBASE_AUTH_EMULATOR_HOST",
    "FIREBASE_CONFIG",
    "GCLOUD_PROJECT",
    "GOOGLE_CLOUD_PROJECT"
  ].map((name) => [name, process.env[name] ?? null])
);

const fixture = {
  source: "official-emulator-discovery-placeholder",
  capturedAt: new Date().toISOString(),
  note:
    "TODO: start firebase-tools emulators, run Web/Admin SDK probes, and replace this placeholder with observed requests/responses.",
  env: envSnapshot,
  steps: []
};

await mkdir(dirname(fixturePath), { recursive: true });
await writeFile(fixturePath, `${JSON.stringify(fixture, null, 2)}\n`);
console.log(`wrote ${fixturePath}`);
