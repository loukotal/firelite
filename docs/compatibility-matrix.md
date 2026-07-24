# Compatibility Matrix

Status legend:

- `captured`: official emulator behavior has a fixture.
- `implemented`: Firelite endpoint exists and is contract-tested.
- `planned`: intentionally not implemented yet.
- `unknown`: needs discovery.

| Service | Surface | Status | Notes |
| --- | --- | --- | --- |
| Auth | `accounts:signUp` password user create | implemented | Project-less Identity Toolkit client paths use the project configured when the emulator starts. |
| Auth | `signInAnonymously` / anonymous `accounts:signUp` | implemented | Anonymous account lookup, refresh, token claims, and deletion are SDK-tested. |
| Auth | `accounts:signInWithPassword` | implemented | Password hash is local-only and not production-compatible. |
| Auth | `accounts:lookup` by ID token/local ID | implemented | Unsigned local JWT-like token, sufficient for local contract flow. |
| Auth | `accounts:delete` | implemented | Supports `localId` or `idToken`. |
| Auth | `accounts:signInWithCustomToken` | implemented | Accepts local unsigned JWT-like tokens or plain local IDs; official fixture capture still needed. |
| Auth | `accounts:signInWithIdp` | implemented | Tolerant provider/raw ID/email flow for Google/OAuth popup paths; official fixture capture still needed. |
| Auth | `accounts:sendOobCode` / `accounts:signInWithEmailLink` | implemented | In-memory single-use email-link OOB codes. |
| Auth | Phone MFA enrollment and sign-in | implemented | Supports v2 enrollment/sign-in start and finalize, inspectable SMS codes, `mfaInfo`, MFA-required password responses, and second-factor token claims. |
| Auth | `/emulator/v1/projects/{project}/verificationCodes` | implemented | Lists pending phone verification sessions and codes for local tests. |
| Auth | Web reCAPTCHA discovery | implemented | Matches firebase-tools fallback behavior: v2 Enterprise config returns structured 501 and v1 params return the emulator fake site key/token. |
| Auth | `/emulator/v1/projects/{project}/oobCodes` | implemented | Local inspection endpoint for email-link tests/debugging. |
| Auth | `/emulator/v1/projects/{project}/accounts` list/reset | implemented | Used for test isolation and fixture comparison. |
| Auth | Admin SDK user management | implemented | Supports create, lookup, list, update (including password and custom claims), and delete flows. Covered by contract tests and the Firebase Admin SDK harness. |
| Auth | import/export | planned | Needed for parity with Emulator Suite workflows. |
| Auth | MFA and deeper provider/OOB parity | planned | Tracked in `docs/auth-emulator-api-surface.md`; add only when real local tests require them. |
| Cloud Tasks | REST create/list/delete task flows | implemented | Supports Firebase Admin SDK emulator REST create calls, in-memory list/get/delete, base64 HTTP body decoding, and bounded local HTTP dispatch. HTTPS targets and lease/pause/purge behavior remain unsupported. |
| Cloud Tasks | Functions task queue dispatch | implemented | In `firelite emulators`, dispatches task queue requests directly to the local functions worker matching the queue/function name. Basic version is synchronous and single-attempt. |
| Pub/Sub | topic/subscription CRUD and publish/pull/acknowledge | implemented | HTTP/JSON emulator subset, in-memory and project-scoped. Full SDK gRPC behavior still needs discovery. |
| Pub/Sub | Functions background event dispatch | implemented | Publish dispatches asynchronously to matching Gen 1 `google.pubsub.topic.publish` and Gen 2 `google.cloud.pubsub.topic.v1.messagePublished` triggers with topic filtering. Push subscriptions remain unsupported. |
| Storage | JSON API media upload/download/list/delete | implemented | In-memory object state with `/upload/storage/v1`, `/storage/v1`, and Firebase `/v0` object paths. Defer XML API and full Firebase Security Rules fidelity. |
| Storage | Firebase Web SDK resumable uploads | implemented | Supports start, chunk upload, offset query, and finalization through `X-Goog-Upload-*`. |
| Storage | Emulator bucket object inspection/reset | implemented | `/emulator/v1/projects/{project}/storage/buckets/{bucket}/objects` supports list/reset for local tests. |
| Storage | Functions object finalize events | implemented | Successful direct and resumable uploads asynchronously dispatch Gen 2 CloudEvents and Gen 1 background events with bucket filtering and custom metadata. |
| Functions | HTTP/callable export discovery and proxying | implemented | `firelite functions` starts a checkout-local Node worker, reads gen1/gen2 metadata, and serves `/{project}/{region}/{function}` URLs. |
| Functions | File-watch reload | implemented | Polls watched source files off the async runtime, restarts the Node worker, and swaps the active registry after successful rediscovery. Use `--no-reload` for immutable CI checkouts. |
| Functions | Background event dispatch | planned | Storage object finalize, Pub/Sub message publish, and Cloud Tasks queue dispatch are implemented. Auth, other Storage event types, and broader Eventarc filter semantics remain planned. |
| Functions | Native Rust worker orchestration | planned | Long-term fast reload path if Node process startup becomes the bottleneck. |
| Emulator Hub | locator metadata endpoints | unknown | Discovery harness should capture hub endpoints expected by SDKs/tools. |
