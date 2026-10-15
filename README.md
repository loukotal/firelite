# Firelite

Firelite is an experimental local emulator system for a small, tested subset of Firebase Emulator Suite-compatible workflows. It is focused on fast startup, fast reload, and low idle overhead for multi-checkout development.

> [!WARNING]
> Firelite is alpha software. It is not an official Firebase, Google, or Firebase Emulator Suite project, and it is not affiliated with or endorsed by Firebase or Google. It implements only selected local emulator behaviors and must not be used as a production Firebase replacement.

## Why Firelite?

- **One backend:** Auth, Storage, Pub/Sub, and Cloud Tasks share a Rust process. Only Functions need a Node worker; no JVM is required.
- **Checkout-local workflows:** choose ports and project IDs per checkout, filter Functions exports, and reload on source changes.
- **Local task API:** create, inspect, and delete Cloud Tasks, with dispatch to local handlers, alongside Storage and Pub/Sub triggers.

Firelite focuses on a compact development stack. The [official suite](https://firebase.google.com/docs/emulator-suite) offers broader Firebase coverage, including Firestore, Realtime Database, Hosting, and Security Rules testing. Fast startup and low idle overhead are Firelite design goals; comparative benchmarks are not yet published.

## Features

- Auth: project-scoped password, anonymous, custom-token, provider, and email-link sign-in; phone MFA; Admin SDK user management and custom claims; optional SQLite user persistence.
- Storage: JSON API and Firebase `/v0` object upload, metadata, download, list, and delete; direct and resumable uploads; Gen 1/Gen 2 object-finalize triggers.
- Pub/Sub: HTTP/JSON topic and subscription CRUD, publish/pull/acknowledge, a unary gRPC subset, and Gen 1/Gen 2 publish triggers.
- Cloud Tasks: in-memory task create/get/list/delete and local HTTP task queue dispatch.
- Functions: checkout-local Node worker with Gen 1/Gen 2 export discovery, HTTP/callable handlers, export filters, and file-watch reload.
- Browser console for Auth users, email action codes, and Storage objects.
- Rust contract tests and Firebase Web, Firebase Admin, and Google Cloud SDK harnesses.

## Requirements

- Rust 1.82 or newer to install or build Firelite with Cargo.
- Node.js to load and execute Cloud Functions; install your functions package's dependencies before starting Firelite.
- npm only for the optional SDK compatibility harness.

Install the CLI from this checkout with `cargo install --path crates/firelite`, or use the `cargo run` commands below.

## Quick start

Install the CLI from crates.io:

```sh
cargo install firelite --locked
```

From the directory containing the compiled Functions `package.json` and entrypoint, start the combined local stack:

```sh
cd /path/to/your-project/functions
firelite emulators \
  --project demo-firelite \
  --host 127.0.0.1 \
  --watch .
```

Firelite loads the Functions worker before opening the listeners. A successful startup prints the discovered function names and the Auth, Storage, Pub/Sub, Cloud Tasks, and Functions listener addresses. TypeScript must already be compiled; Firelite loads the package entrypoint but does not run the application's build command.

Point local SDKs at the listeners:

```sh
export FIREBASE_AUTH_EMULATOR_HOST=127.0.0.1:9099
export FIREBASE_STORAGE_EMULATOR_HOST=127.0.0.1:9199
export STORAGE_EMULATOR_HOST=http://127.0.0.1:9199
export PUBSUB_EMULATOR_HOST=127.0.0.1:8085
export CLOUD_TASKS_EMULATOR_HOST=127.0.0.1:9899
export GCLOUD_PROJECT=demo-firelite
```

Use `firelite functions` instead when only the checkout-local Functions worker is needed:

```sh
firelite functions --project demo-firelite --watch . --port 5001
```

HTTP functions are served at:

```text
http://127.0.0.1:5001/{project}/{region}/{functionName}
```

The proxy preserves the function's response status and body. For example, a function that forwards an upstream `404` returns that `404`; a Functions worker transport failure is reported separately as `502 Bad Gateway`.

For Auth without a Functions worker, run the standalone daemon:

```sh
firelite daemon --project demo-firelite --host 127.0.0.1 --port 9099
```

To keep Auth users between runs, pass `--persist .firelite/state.sqlite` to `daemon` or `emulators`. Reset one project's persisted Auth users with:

```sh
firelite reset --project demo-firelite --persist .firelite/state.sqlite
```

Magic sign-in links, email-verification links, and phone verification codes are printed prominently in the emulator console. Interactive terminals use cyan, magenta, and yellow highlighting; redirected output and `NO_COLOR` remain plain text unless `FORCE_COLOR` is set. Pending values are also available from `/emulator/v1/projects/{project}/oobCodes` and `/emulator/v1/projects/{project}/verificationCodes`.

Example REST call:

```sh
curl -s 'http://127.0.0.1:9099/identitytoolkit.googleapis.com/v1/accounts:signUp?key=fake' \
  -H 'content-type: application/json' \
  -d '{"email":"alice@example.test","password":"secret123","returnSecureToken":true}'
```

## CLI shape

```sh
firelite daemon
firelite reset --project demo-myrepo-agent-17 --persist .firelite/state.sqlite
firelite functions --project demo-myrepo-agent-17 --watch ./functions --port 5001
firelite emulators --project demo-myrepo-agent-17 --watch ./functions
firelite emulators --project demo-myrepo-agent-17 --watch ./functions --filter api
firelite emulators --project demo-myrepo-agent-17 --watch ./functions --no-reload
```

`daemon` serves Auth, Storage, Pub/Sub, Cloud Tasks, and the console on one port, without a Functions worker. Its default project comes from `--project`, then `GCLOUD_PROJECT`, then `GOOGLE_CLOUD_PROJECT`, falling back to `demo-firelite`.

`functions` runs a checkout-local Node worker supervisor for HTTP/callable Cloud Functions exports at `/{project}/{region}/{function}` and reloads it when watched files change. Build TypeScript functions before Firelite loads the functions directory. `reset` removes one project's Auth users from the SQLite database selected by `--persist`.

`emulators` runs Auth, Storage, Pub/Sub, Cloud Tasks, and Functions together. By default it listens on the local setup ports: Auth on `127.0.0.1:9099`, Storage on `127.0.0.1:9199`, Pub/Sub on `127.0.0.1:8085`, Cloud Tasks on `127.0.0.1:9899`, and Functions on `127.0.0.1:5001`. The listeners share the same state. `--persist <FILE>` persists Auth users; other service state remains in memory.

Listener ports come from Firelite's CLI flags. Firelite does not read `firebase.json` or `firebase.local.json`, so a project wrapper should translate its configured ports into `--auth-port`, `--storage-port`, `--pubsub-port`, `--tasks-port`, and `--functions-port` explicitly.

Open the console on the Auth port at [http://127.0.0.1:9099/__/ui](http://127.0.0.1:9099/__/ui). Set its project and bucket fields to match your application. You can create, list, and delete users and objects, inspect email action codes, and reset Auth users or a Storage bucket.

`STORAGE_EMULATOR_HOST` configures `@google-cloud/storage`. For Firebase Web SDK clients, use `connectAuthEmulator`, `connectStorageEmulator`, and `connectFunctionsEmulator` with the corresponding addresses. When using `daemon`, point all backend service clients at its single port instead.

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

Storage supports direct and resumable uploads through the Firebase Web and Google Cloud Storage SDKs. In `firelite emulators`, completed uploads also dispatch object-finalize events asynchronously to matching Gen 1 and Gen 2 Functions, with bucket filtering and custom metadata.

Pub/Sub supports HTTP/JSON topic/subscription CRUD and publish, pull, and acknowledge flows. Its unary gRPC subset covers `CreateTopic`, `Publish`, `CreateSubscription`, `Pull`, and `Acknowledge`; the Node `@google-cloud/pubsub` harness exercises topic/subscription creation, publishing, and unary pull. Streaming pull and push subscriptions are unsupported. In `firelite emulators`, publishing also asynchronously invokes matching Gen 1 and Gen 2 Pub/Sub functions; this background dispatch does not require a subscription.

Cloud Tasks accepts Firebase Admin SDK task queue `createTask` calls at `CLOUD_TASKS_EMULATOR_HOST=127.0.0.1:9899`. In `firelite emulators`, enqueued task queue requests are dispatched directly to the checkout-local functions worker when the filter matches the queue/function name. In the basic implementation, dispatch is synchronous and runs before the create-task response returns. Dispatch targets are intentionally limited to local `http://` URLs so the runtime does not carry a TLS stack.

For a checkout with custom ports, run the installed binary from its Functions directory and pass those ports explicitly. This is the recommended shape for project-local development scripts and CI:

```sh
firelite emulators \
  --project demo-myrepo-agent-17 \
  --host 127.0.0.1 \
  --auth-port 9099 \
  --storage-port 9199 \
  --pubsub-port 8085 \
  --tasks-port 9499 \
  --functions-port 5001 \
  --watch . \
  --filter api \
  --filter platformApi \
  --filter staticIpApiProxy \
  --filter e2e
```

Add `--no-reload` for immutable CI builds. During local development, keep the application's compiler/watch process running alongside Firelite so changes reach the compiled entrypoint before Firelite reloads the worker.

To run an unpublished Firelite checkout instead, replace `firelite` with:

```sh
cargo run --manifest-path /path/to/firelite/Cargo.toml -p firelite --
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
npm run test:functions
npm run test:storage-sdk
npm run test:pubsub-sdk
```

The harness starts Firelite on temporary loopback ports. `test:auth` runs both Firebase Web and Admin Auth tests; `test:storage-sdk` runs Firebase Web and Google Cloud Storage tests; `test:pubsub-sdk` runs Google Cloud Pub/Sub tests.

## Project Status

Firelite implements a local compatibility subset. Firestore, Realtime Database, Hosting, full Security Rules evaluation, Auth import/export, Auth background triggers, and Storage events other than object finalize are outside the current feature set. Cloud Tasks dispatch is single-attempt and does not implement production scheduling/retry semantics.

See:

- `docs/compatibility-matrix.md` for supported, planned, and unknown surfaces.
- `docs/auth-emulator-api-surface.md` for Auth compatibility notes.
- `docs/architecture.md` for the process model and compatibility strategy.

## License

MIT.
