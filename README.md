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
firelite reset --project demo-myrepo-agent-17
firelite functions --project demo-myrepo-agent-17 --watch ./functions --port 5001
firelite emulators --project demo-myrepo-agent-17 --watch ./functions
firelite emulators --project demo-myrepo-agent-17 --watch ./functions --filter api
firelite emulators --project demo-myrepo-agent-17 --watch ./functions --no-reload
```

`daemon` runs the shared Auth-compatible backend. `functions` runs a checkout-local Node worker supervisor for HTTP/callable Cloud Functions exports and reloads it when watched files change. TypeScript functions should be built by the surrounding test/dev workflow before Firelite loads the functions directory. `reset` is still present to lock the UX surface and will be wired to per-project state reset in later milestones.

`emulators` runs Auth, Storage, Pub/Sub, Cloud Tasks, and Functions together. By default it listens on the local setup ports: Auth on `127.0.0.1:9099`, Storage on `127.0.0.1:9199`, Pub/Sub on `127.0.0.1:8085`, Cloud Tasks on `127.0.0.1:9899`, and Functions on `127.0.0.1:5001`. The listeners share the same in-memory state.

For CI runs where function source does not change after startup, pass `--no-reload` to skip the file polling task. The combined runner loads and validates the initial Functions worker before opening the other emulator listeners. The Functions listener reports worker liveness at `/__/health`.

Terminal logs are intentionally compact: Firelite keeps timestamps but omits repeated level labels and per-line worker metadata. Known request-context dumps are reduced to path/type/status/duration summaries, while application stack traces retain their original shape. Interactive terminals color HTTP methods and response status fields; redirected CI logs remain free of ANSI escapes. Set `RUST_LOG=debug` or `RUST_LOG=firelite=debug` when deeper diagnostics are needed.

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

Cloud Tasks accepts Firebase Admin SDK task queue `createTask` calls at `CLOUD_TASKS_EMULATOR_HOST=127.0.0.1:9899`. In `firelite emulators`, enqueued task queue requests are dispatched directly to the checkout-local functions worker when the filter matches the queue/function name. In the basic implementation, dispatch is synchronous and runs before the create-task response returns. Dispatch targets are intentionally limited to local `http://` URLs so the runtime does not carry a TLS stack.

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

## Release

Releases are published by GitHub Actions when a GitHub Release is created. The repository must have a `CRATES_TOKEN` Actions secret.

Use the release helper from a clean `main` branch:

```sh
scripts/release.sh 0.3.0
```

The script bumps the crate version, refreshes `Cargo.lock`, runs local checks, commits, tags `v0.3.0`, pushes `main` and the tag, and creates the GitHub Release that triggers publishing.

## Project Status

Firelite is intentionally incomplete. See:

- `docs/compatibility-matrix.md` for supported, planned, and unknown surfaces.
- `docs/auth-emulator-api-surface.md` for Auth compatibility notes.
- `docs/architecture.md` for the process model and compatibility strategy.

## License

MIT.
