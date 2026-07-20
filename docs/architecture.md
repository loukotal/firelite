# Architecture

Firelite separates checkout-specific process work from shared stateful emulator backends.

## Process model

- One lightweight daemon can host stateful emulator-compatible services for many projects.
- Every checkout uses a unique project ID, preferably `demo-*`.
- Auth, Pub/Sub, Tasks, and Storage state is namespaced by project ID inside the daemon.
- Cloud Functions workers are per checkout because they execute checkout-local source code and dependencies.
- Function reloads should restart only the affected checkout workers, not shared backend services.

## Modules

- `firelite daemon`: shared backend process for stateful emulator-compatible services.
- `firelite reset`: removes one project's persisted Auth users; reset support for other services is planned.
- `firelite functions`: checkout-local function worker supervisor. It starts a Node worker, discovers Firebase Functions exports from the watched source directory, proxies emulator-compatible HTTP function URLs, and restarts the worker when source files change.
- `firelite emulators`: combined local runner for stateful services plus one checkout-local functions worker. Cloud Tasks dispatch is wired directly to that local worker. Stateful listeners open only after the initial Functions worker is ready.
- `auth`: project-scoped emulator state with optional SQLite user persistence through `--persist`.
- `storage`, `pubsub`, and `tasks`: in-memory, project-scoped emulator state.

The Functions supervisor monitors both source changes and the Node child process. Unexpected worker exits temporarily mark Functions unhealthy and trigger bounded-backoff restarts. CI runs that do not edit function source can use `--no-reload` to avoid polling the checkout.

## Compatibility strategy

Compatibility is defined by observed SDK/emulator behavior, not by production API completeness. The discovery harness runs official emulators and real SDK probes, captures request/response/event fixtures, and converts those fixtures into Rust contract tests.

Each supported endpoint should have:

- observed official emulator fixture,
- Rust emulator contract test,
- documented env vars and project scoping assumptions,
- explicit unsupported cases.

## State

Auth is in-memory by default for fast tests. Passing `--persist <FILE>` loads users from SQLite and writes user changes back after Auth requests. Email, phone, and provider indexes are rebuilt from user records at startup. Short-lived verification codes and pending MFA credentials are intentionally not restored. Storage, Pub/Sub, and Tasks remain in-memory.

## Tracing

Firelite uses tracing internally but renders compact terminal output with timestamps and without repeated level labels. Functions worker output is forwarded without per-line generation/source prefixes; known request-context dumps are collapsed to path/type/status/duration summaries. Request IDs and event IDs should be propagated through SDK-facing HTTP/gRPC handlers, internal queues, and Functions delivery paths as those services are implemented, but stay out of the default human-facing output.
