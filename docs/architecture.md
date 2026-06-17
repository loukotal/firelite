# Architecture

Firelite separates checkout-specific process work from shared stateful emulator backends.

## Process model

- One lightweight daemon can host stateful emulator-compatible services for many projects.
- Every checkout uses a unique project ID, preferably `demo-*`.
- Auth, Pub/Sub, Tasks, and Storage state is namespaced by project ID inside the daemon.
- Cloud Functions workers are per checkout because they execute checkout-local source code and dependencies.
- Function reloads should restart only the affected checkout workers, not shared backend services.

## Modules

- `firelite daemon`: shared backend process and future control plane.
- `firelite attach`: planned registration of `{ project_id, workdir, ports, env }`.
- `firelite reset`: planned per-project state reset across all enabled services.
- `firelite functions`: planned checkout-local function worker supervisor.
- `auth`: first implemented service, currently in-memory only.

## Compatibility strategy

Compatibility is defined by observed SDK/emulator behavior, not by production API completeness. The discovery harness runs official emulators and real SDK probes, captures request/response/event fixtures, and converts those fixtures into Rust contract tests.

Each supported endpoint should have:

- observed official emulator fixture,
- Rust emulator contract test,
- documented env vars and project scoping assumptions,
- explicit unsupported cases.

## State

The first Auth implementation is in-memory for fast tests. The durable daemon state layer should be introduced behind service traits after the first contract surface stabilizes. SQLite is the preferred first durable format because it is inspectable, widely available, and supports simple per-project reset transactions.

## Tracing

The daemon uses structured tracing. Request IDs and event IDs should be propagated through SDK-facing HTTP/gRPC handlers, internal queues, and Functions delivery paths as those services are implemented.
