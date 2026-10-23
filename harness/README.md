# Discovery Harness

This directory holds the official-emulator compatibility discovery harness. It is intentionally separate from the Rust emulator implementation.

## Goal

Run official Firebase emulators with real Firebase Web/Admin/Google SDK probes, capture the observed local protocol, and turn those observations into stable contract fixtures.

## Planned flow

`discover:auth` currently writes a placeholder fixture; it does not start official emulators. The SDK E2E scripts below launch Firelite directly, so this package does not install the unused `firebase-tools` CLI.

```sh
npm install
npm run discover:auth
```

The harness should:

- start `firebase emulators:start` for selected services,
- configure `demo-*` project IDs and emulator env vars,
- execute SDK operations,
- capture HTTP/gRPC requests and responses where possible,
- read emulator hub metadata,
- store normalized fixtures under `fixtures/official/`,
- avoid copying or decompiling emulator binaries.

## Env vars to capture

- `FIREBASE_AUTH_EMULATOR_HOST`
- `FIRESTORE_EMULATOR_HOST`
- `FIREBASE_STORAGE_EMULATOR_HOST`
- `PUBSUB_EMULATOR_HOST`
- `CLOUD_TASKS_EMULATOR_HOST`
- `FUNCTIONS_EMULATOR`
- `FIREBASE_CONFIG`
- `GCLOUD_PROJECT`
- `GOOGLE_CLOUD_PROJECT`

## First probes

- Auth create user with email/password.
- Auth sign in with email/password.
- Auth list users through Admin SDK.
- Auth delete user.
- Auth reset/import/export endpoints.

## SDK E2E

Run Firelite against the real Firebase Web Auth SDK:

```sh
npm run test:auth-sdk
```

This starts `cargo run -p firelite -- daemon` on a temporary loopback port, calls `connectAuthEmulator`, and verifies password, custom-token, and email-link sign-in flows through `firebase/auth`.

Run Firelite against the real Firebase Admin Auth SDK:

```sh
npm run test:auth-admin-sdk
```

This verifies `createUser`, `getUser`, `getUserByEmail`, `listUsers`, and `deleteUser` through `firebase-admin/auth`.

## Dependency security

Use the Node version pinned in `../.node-version`, then `npm ci` and `npm audit`.

The scoped `@grpc/grpc-js` override keeps Firebase Web SDK's Firestore dependency on a patched 1.x release. Firebase pins it to `~1.9.0`, which is affected by GHSA-m9gg-hp2v-232j and GHSA-f596-whhp-79r4. Remove the override when Firebase's native range resolves to `>=1.13.6` (1.13.x) or `>=1.14.5` (1.14.x), and a fresh install passes the audit.
