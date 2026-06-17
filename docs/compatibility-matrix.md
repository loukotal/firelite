# Compatibility Matrix

Status legend:

- `captured`: official emulator behavior has a fixture.
- `implemented`: Firelite endpoint exists and is contract-tested.
- `planned`: intentionally not implemented yet.
- `unknown`: needs discovery.

| Service | Surface | Status | Notes |
| --- | --- | --- | --- |
| Auth | `accounts:signUp` password user create | implemented | In-memory, default project `demo-firelite` for Identity Toolkit paths. |
| Auth | `accounts:signInWithPassword` | implemented | Password hash is local-only and not production-compatible. |
| Auth | `accounts:lookup` by ID token/local ID | implemented | Unsigned local JWT-like token, sufficient for local contract flow. |
| Auth | `accounts:delete` | implemented | Supports `localId` or `idToken`. |
| Auth | `accounts:signInWithCustomToken` | implemented | Accepts local unsigned JWT-like tokens or plain local IDs; official fixture capture still needed. |
| Auth | `accounts:signInWithIdp` | implemented | Tolerant provider/raw ID/email flow for Google/OAuth popup paths; official fixture capture still needed. |
| Auth | `accounts:sendOobCode` / `accounts:signInWithEmailLink` | implemented | In-memory single-use email-link OOB codes. |
| Auth | `/emulator/v1/projects/{project}/oobCodes` | implemented | Local inspection endpoint for email-link tests/debugging. |
| Auth | `/emulator/v1/projects/{project}/accounts` list/reset | implemented | Used for test isolation and fixture comparison. |
| Auth | import/export | planned | Needed for parity with Emulator Suite workflows. |
| Auth | MFA and deeper provider/OOB parity | planned | Tracked in `docs/auth-emulator-api-surface.md`; add only when real local tests require them. |
| Cloud Tasks | REST create/list/delete/lease task flows | planned | Next bounded target after Auth fixtures. |
| Cloud Tasks | Functions task queue dispatch | unknown | Needs official emulator event capture. |
| Pub/Sub | topic/subscription CRUD and publish/pull | planned | Implement SDK-compatible subset before full behavior. |
| Pub/Sub | push delivery to Functions emulator | unknown | Needs event flow capture. |
| Storage | JSON/XML object upload/download/list/delete | planned | Defer full Firebase Security Rules fidelity. |
| Storage | Functions object events | unknown | Needs event flow capture. |
| Functions | Official emulator integration | planned | Per-checkout process manager first. |
| Functions | Native Rust worker orchestration | planned | Long-term fast reload path. |
| Emulator Hub | locator metadata endpoints | unknown | Discovery harness should capture hub endpoints expected by SDKs/tools. |
