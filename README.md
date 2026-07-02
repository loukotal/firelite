# Firelite

Firelite is an experimental local emulator system for a small, tested subset of Firebase Emulator Suite-compatible workflows. It is focused on fast startup, fast reload, and low idle overhead for multi-checkout development.

> [!WARNING]
> Firelite is alpha software. It is not an official Firebase, Google, or Firebase Emulator Suite project, and it is not affiliated with or endorsed by Firebase or Google. It implements only selected local emulator behaviors and must not be used as a production Firebase replacement.

The immediate goal is to discover local SDK/emulator contracts, capture them as fixtures, and implement only the compatibility surface needed for local tests and checkout-specific Cloud Functions workflows.

## Features

- Rust workspace and `firelite` CLI.
- Auth emulator state namespaced by Firebase project ID.
- Storage emulator state for common JSON API and `/v0` object paths.
- Pub/Sub emulator state for HTTP/JSON topic, subscription, publish, pull, and acknowledge flows.
- Cloud Tasks emulator state for task create/list/delete and local task queue dispatch.
- Checkout-local Cloud Functions supervisor with Node handler discovery and reload.
- Contract tests and SDK harnesses for supported Auth and Storage flows.
- Architecture and compatibility notes in `docs/`.

## Requirements

- Rust 1.82 or newer.
- Node.js and npm for the optional SDK compatibility harness.

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
firelite attachments
firelite reset --project demo-myrepo-agent-17
firelite functions --project demo-myrepo-agent-17 --watch ./functions --port 5001
firelite functions --project demo-myrepo-agent-17 --watch ./functions --build-command 'npm run build'
firelite emulators --project demo-myrepo-agent-17 --watch ./functions
firelite emulators --project demo-myrepo-agent-17 --watch ./functions --filter api
```

`daemon` runs the shared Auth-compatible backend. `functions` runs a checkout-local Node worker supervisor for HTTP/callable Cloud Functions exports and reloads it when watched files change. For TypeScript functions, pass the same SWC/tsc build command the Firebase emulator expects; Firelite runs it before the initial load and before each reload. `attach` registers a running checkout-local functions worker with the daemon control plane; `attachments` lists registered workers. `reset` is still present to lock the UX surface and will be wired to the daemon control plane in later milestones.

`emulators` runs Auth, Storage, Pub/Sub, Cloud Tasks, and Functions together. By default it listens on the local setup ports: Auth on `127.0.0.1:9099`, Storage on `127.0.0.1:9199`, Pub/Sub on `127.0.0.1:8085`, Cloud Tasks on `127.0.0.1:9899`, and Functions on `127.0.0.1:5001`. The listeners share the same in-memory state.

Example:

```sh
cargo run -p firelite -- \
  emulators \
  --project demo-myrepo-agent-17 \
  --host 127.0.0.1 \
  --auth-port 9099 \
  --storage-port 9199 \
  --pubsub-port 8085 \
  --tasks-port 9899 \
  --functions-port 5001 \
  --watch ./functions \
  --filter api
```

Pass `--filter` to run only selected Cloud Functions exports/names. It can be repeated, for example `--filter api --filter e2e`.

Pub/Sub accepts HTTP/JSON emulator calls at `PUBSUB_EMULATOR_HOST=127.0.0.1:8085` for topic/subscription create, publish, pull, and acknowledge flows.

Cloud Tasks accepts Firebase Admin SDK task queue `createTask` calls at `CLOUD_TASKS_EMULATOR_HOST=127.0.0.1:9899`. Enqueued task queue requests are dispatched to the attached functions worker whose filter matches the queue/function name. In the basic implementation, dispatch is synchronous and runs before the create-task response returns.

To attach separately started functions workers to a daemon:

```sh
# terminal 1
firelite daemon --host 127.0.0.1 --port 9099

# terminal 2: starts a functions worker and registers it with the daemon
firelite functions \
  --project demo-myrepo-agent-17 \
  --watch ./functions \
  --port 5001 \
  --filter api \
  --attach

# terminal 3: optional second worker on another port
firelite functions \
  --project demo-myrepo-agent-17 \
  --watch ./functions \
  --port 5002 \
  --filter e2e \
  --attach

firelite attachments

# The daemon can now proxy attached function routes:
curl http://127.0.0.1:9099/demo-myrepo-agent-17/us-central1/api
```

To run Firelite from another checkout, execute Cargo from the project or
functions directory and point `--manifest-path` at this repository:

```sh
cargo run --manifest-path /Users/louky/Documents/firelite/Cargo.toml -p firelite -- \
  emulators \
  --project bf-demo-a24dc \
  --host 127.0.0.1 \
  --auth-port 9099 \
  --storage-port 9199 \
  --pubsub-port 8085 \
  --tasks-port 9899 \
  --functions-port 5001 \
  --watch . \
  --filter api \
  --filter e2e
```

## Development

Run the Rust test suite:

```sh
cargo test
```

Run the optional Firebase SDK compatibility harness:

```sh
cd harness
npm install
npm run test:auth
npm run test:auth-admin-sdk
npm run test:storage-sdk
```

The harness starts Firelite on temporary loopback ports and verifies supported flows with the official Firebase Web and Admin SDK packages.

## Project Status

Firelite is intentionally incomplete. See:

- `docs/compatibility-matrix.md` for supported, planned, and unknown surfaces.
- `docs/auth-emulator-api-surface.md` for Auth compatibility notes.
- `docs/architecture.md` for the process model and compatibility strategy.

## License

MIT.
