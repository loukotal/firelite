# Firelite

Firelite is an experimental, unofficial Firebase Emulator Suite-compatible local emulator system focused on fast startup, fast reload, and low idle overhead for multi-checkout agent development.

It is not a Firebase product and does not try to clone production Firebase. The immediate goal is to discover the local SDK/emulator contracts, capture them as fixtures, and implement only the compatibility surface needed for local tests and checkout-specific Cloud Functions workflows.

## Current milestone

- Rust workspace and `firelite` CLI.
- Minimal Auth emulator state namespaced by Firebase project ID.
- Checkout-local Cloud Functions supervisor with Node handler discovery and reload.
- Contract fixtures for basic Auth create, sign-in, list, and delete flows.
- Compatibility harness scaffolding for running official Firebase emulators and SDK probes.
- Architecture and compatibility notes in `docs/`.

## Quick start

```sh
cargo run -p firelite -- daemon --host 127.0.0.1 --port 9099
```

Then point SDKs at the Auth emulator:

```sh
export FIREBASE_AUTH_EMULATOR_HOST=127.0.0.1:9099
export GCLOUD_PROJECT=demo-firelite
```

Example REST call:

```sh
curl -s 'http://127.0.0.1:9099/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake' \
  -H 'content-type: application/json' \
  -d '{"email":"alice@example.test","password":"secret123","returnSecureToken":true}'
```

## CLI shape

```sh
firelite daemon
firelite attach --project demo-myrepo-agent-17 --workdir ./checkout-17
firelite reset --project demo-myrepo-agent-17
firelite functions --project demo-myrepo-agent-17 --watch ./functions --port 5001
firelite functions --project demo-myrepo-agent-17 --watch ./functions --build-command 'npm run build'
```

`daemon` runs the shared Auth-compatible backend. `functions` runs a checkout-local Node worker supervisor for HTTP/callable Cloud Functions exports and reloads it when watched files change. For TypeScript functions, pass the same SWC/tsc build command the Firebase emulator expects; Firelite runs it before the initial load and before each reload. `attach` and `reset` are still present to lock the UX surface and will be wired to the daemon control plane in later milestones.
