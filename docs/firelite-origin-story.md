---
marp: true
theme: default
paginate: true
title: How Firelite Got Built
description: A short build log reconstructed from Codex threads and Git history
style: |
  section {
    font-family: Inter, ui-sans-serif, system-ui, sans-serif;
    color: #172033;
    background: #f7f8f3;
    padding: 64px 76px;
  }
  h1, h2 { color: #c53a24; }
  h1 { font-size: 58px; }
  h2 { font-size: 42px; }
  p, li { font-size: 27px; line-height: 1.35; }
  strong { color: #111827; }
  code { color: #8f291c; }
  blockquote {
    border-left: 8px solid #f6b73c;
    padding-left: 24px;
    color: #334155;
  }
  footer { color: #64748b; }
---

# How Firelite Got Built

### From a multi-checkout bottleneck to a focused Rust emulator toolkit

*A short build log from Codex threads + Git history*

<!-- Sources: initial Firelite thread 019ed64a-f8f0-70a3-91f6-4ddfcd85f024; Git history beginning 2026-06-17. -->

---

## The problem came before the product

Parallel AI agents meant **many isolated checkouts** of the same Firebase project.

The official emulator stack was too expensive to start and keep idle for every checkout.

> Keep stateful services light and project-scoped. Keep Functions checkout-local.

That constraint became Firelite's architecture.

<!-- Sources: initial project brief in thread 019ed64a-f8f0-70a3-91f6-4ddfcd85f024; docs/architecture.md. -->

---

## The first move was to learn the real contract

Firelite did not begin as a complete Firebase clone.

1. Run official emulators
2. Exercise them with real Firebase Web, Admin, and Google SDKs
3. Capture behavior as fixtures
4. Turn those observations into Rust contract tests

**Compatibility was defined by behavior that local tests actually needed.**

<!-- Sources: initial project brief and SDK-testing follow-ups in thread 019ed64a-f8f0-70a3-91f6-4ddfcd85f024; README.md; docs/architecture.md. -->

---

## Separate threads built vertical slices

- **Auth:** scaffold first, then expand from real SDK flows and an explicit API checklist
- **Functions:** discover exports, proxy HTTP/callable handlers, support ESM, watch files, reload one worker
- **Storage:** implement common object paths, then verify with Node SDKs
- **Developer experience:** compact Firebase logs, a small management UI, and one combined `emulators` command

The pieces were repeatedly committed, tested, and tried from another checkout.

<!-- Sources: threads 019ed64a-f8f0-70a3-91f6-4ddfcd85f024, 019ed656-bb4c-7c33-be76-8a21a90d4cb8, 019ed65b-8bd7-74e3-9032-eebd812da5e3, 019ed682-2a81-7ad3-aad2-49e0c203f40d, 019ed682-4b6a-7b83-8a00-c4d61314386f, and 019ed682-953f-7702-bdec-a017cb292142. -->

---

## A real app exposed what toy tests missed

Firelite was pointed at a large functions repo with hundreds of handlers.

That surfaced the useful failures:

- browser CORS and project routing
- TypeScript build output and ESM loading
- Cloud Tasks dispatch and background events
- resumable Storage uploads
- MFA, token, and Admin Auth parity
- a cached binary that could not find its Node worker in CI

Each failure became a smaller compatibility contract or a simpler runtime rule.

<!-- Sources: Firelite development thread 019ed682-2a81-7ad3-aad2-49e0c203f40d; CI diagnosis thread 019f3657-79e1-74b2-8139-f38f6acd3c23; Git history. -->

---

## The result is narrower—and better for CI

- `firelite emulators` became the main CI path
- external `attach` plumbing and `--build-command` were removed
- Functions must be ready before listeners are considered healthy
- `--no-reload` removes polling in immutable CI checkouts
- compact logs preserve failures without drowning the test output

**Five emulator surfaces · one checkout-tested CI path**

The goal is not to clone Firebase. It is to make the local tests that matter fast and dependable.

<!-- Sources: CI simplification thread 019f436d-cc5e-75e2-b734-65310095c61f; README.md; docs/architecture.md; docs/compatibility-matrix.md. -->
