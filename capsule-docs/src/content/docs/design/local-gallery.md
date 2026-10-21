---
title: Local Gallery
description: The owner contract for offline-first native clients — every native app is a complete local gallery with no server
status: draft
---

Every native app is a **complete local gallery first** and a synced client second. Mechanisms live in [Filesystem — Client](/design/filesystem/client/) and [Clients](/design/clients/); this doc owns the requirements.

## Functional Requirements

- **FR1** — Browse, search, view locally-held assets, and organize with zero connectivity; none of these has a server dependency.
- **FR2** — Never signing in is a valid mode: import, organize, search, export.
- **FR3** — [Import](/design/import/pipeline/) is local; [streaming import](/design/import/pipeline/#import-upload-streaming-mode) is the one server-requiring exception.
- **FR4** — An unavailable remote representation degrades along the [fetch ladder](/design/transfer/download/#tiered-on-demand-fetch); no offline condition removes an asset, album, or index entry.
- **FR5** — A damaged index rebuilds from local sidecars ([maintenance](/design/filesystem/maintenance/)) without network.

## Security Requirements

- **SR1** — The Recently Deleted and Hidden views require fresh local authentication (biometric, else credential), with a short per-view grace window. This is UX protection, not a cryptographic boundary.
- **SR2** — At rest, mobile relies on the app sandbox plus file-based encryption; desktop on OS user permissions and full-disk encryption only. Rooted devices void the sandbox guarantee.
- **SR3** — Decoded previews, caches and temp files stay inside the app-private library root ([placement](/design/filesystem/client/#mobile-clients)); export applies the [privacy strip](/design/metadata/#privacy-on-export).

## Non-Functional Requirements

- **NFR1** — Gallery read paths attempt zero network I/O; tested with a socket-refusing harness.
- **NFR2** — An airplane-mode, never-enrolled import → browse → search → organize → export run is part of the [E2E surface](/design/module-map/#e2e-test-surface).
- **NFR3** — Offline cold start takes the online code path; no blocking connectivity probe.

## Validation

Unit tests cover the gated-view policy and cache placement; smoke tests cover NFR1, FR2 and FR5 offline.
