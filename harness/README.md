# Discovery Harness

This directory holds the official-emulator compatibility discovery harness. It is intentionally separate from the Rust emulator implementation.

## Goal

Run official Firebase emulators with real Firebase Web/Admin/Google SDK probes, capture the observed local protocol, and turn those observations into stable contract fixtures.

## Planned flow

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
