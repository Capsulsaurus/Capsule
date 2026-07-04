---
title: Local Gallery
description: The owner contract for offline-first native clients — every native app is a complete local gallery with no server
status: draft
---

Every native Capsule app is a **complete local gallery first** and a synced client second. This doc owns that requirement set — the functional and non-functional requirements that make "works with no server" a testable contract rather than a slogan. The mechanisms live where they always did — the local index in [Filesystem — Client](/design/filesystem/client/), the client-side query surface in [API Surfaces](/design/api-surfaces/#surface--transport-map), client duties in [Clients](/design/clients/) — this doc owns the *requirements* and links the mechanisms.

The [offline/online divide principle](/design/principles/#principles) is the root: a feature works either solely online or solely offline, never differently by connectivity. This doc pins down which side of the divide the gallery sits on: **all of it is offline**.

## Functional Requirements

- **FR1 — Full gallery offline.** Browse (timeline, albums, stacks), search, filter, view full-resolution locally-held assets, and organize (albums, tags, ratings, [culling flags](/design/organization/), hidden) with zero connectivity. Every one of these reads and writes `library.sqlite` + the local blob store ([Filesystem — Client](/design/filesystem/client/)); none has a server dependency by construction ([rich queries have no server surface at all](/design/api-surfaces/#surface--transport-map)).
- **FR2 — Never-signed-in is a valid mode.** A user who installs a native app and never connects a server has a fully functional local photo library: import, organize, search, export. Sync is an *addition* to a working product, not its precondition. (The crypto data plane operates locally; enrollment happens when — if ever — a server is added.)
- **FR3 — Import works offline.** The scan → plan → execute [import pipeline](/design/import/pipeline/) is entirely local; upload is a separate, later concern. (The one deliberate exception: [streaming import-upload mode](/design/import/pipeline/#import-upload-streaming-mode) exists *because* the device cannot hold the bytes — it requires a server by definition.)
- **FR4 — Degradation is visible, never structural.** When connectivity exists but a *remote-only* representation is unavailable, the asset stays listed with its best local representation and a non-destructive unavailability state ([the degrade ladder](/design/import/download-sync/#tiered-on-demand-fetch)). No offline condition ever removes an asset, an album, or an index entry.
- **FR5 — Recovery without a server.** A damaged local index is rebuilt from local sidecars alone ([recovery-first rebuild](/design/filesystem/maintenance/)); the rebuild requires no network.

## Security Requirements

- **SR1 — Gated views require fresh local auth.** Opening the **Recently Deleted** (trash) view or the **Hidden** view requires fresh local authentication — biometric where enrolled (Face ID / Touch ID / BiometricPrompt), else the device or account credential. One grant covers a short grace window (default 5 minutes, per-view), after which re-auth is required. The gate is view-time UX protection against a borrowed-unlocked-phone snoop; it is **not** a cryptographic boundary (the same data is reachable through the filesystem by anyone who can defeat the platform sandbox — see SR2 for what actually protects bytes at rest).
- **SR2 — At-rest posture per platform, stated honestly.**
  | Platform | Store | Protection from other apps / the user's file browser |
  | --- | --- | --- |
  | iOS | App-private container | Sandbox denies other apps; not visible in Files unless exported; file-level Data Protection (FBE) at rest |
  | Android | `Context` private storage (`filesDir`) | Sandbox + SELinux deny other apps and `content://` browsing; File-Based Encryption at rest |
  | Desktop (macOS/Windows/Linux) | User-directory library ([layout](/design/filesystem/client/#desktop-library-layout)) | OS user permissions only — any process of the same user can read it. Full-disk encryption is the at-rest story; we do not pretend otherwise. |

  On mobile, library content therefore **is** protected from the user's own casual file access and from other apps — by platform sandbox, which is the strongest guarantee actually available. Rooted/jailbroken devices void it; we do not claim otherwise.
- **SR3 — No plaintext spillage outside the library root.** Decoded previews, caches, and temp files live inside the app-private library root ([placement rules](/design/filesystem/client/#mobile-clients)), never in shared/world-readable locations. Export is the only sanctioned way bytes leave the root, and it applies the [privacy-on-export strip](/design/metadata/#privacy-on-export).

## Non-Functional Requirements

- **NFR1 — No network on read paths (assertable).** Gallery read paths (timeline, album, search, asset view of locally-held tiers) perform zero network I/O — not "tolerate failure," but *do not attempt*. This is testable by construction: run the read-path suite with networking disabled and assert no socket is ever opened.
- **NFR2 — Airplane-mode E2E.** The bounded E2E surface includes one case: import → browse → search → organize → export on a device in airplane mode from first launch (never enrolled). It must behave identically to the connected case minus sync surfaces.
- **NFR3 — Offline startup is not a degraded startup.** Cold start offline takes the same code path as cold start online; there is no blocking connectivity probe on the startup path.

## Validation

- **Unit:** gated-view policy (grace window expiry, per-view independence, biometric-fallback-to-credential); placement rules for every cache/temp write (path is under the library root).
- **Smoke:** read-path network assertion (NFR1) with a socket-refusing harness; index rebuild offline (FR5); never-enrolled library round-trip (FR2).
- **E2E:** the airplane-mode case (NFR2), listed in the [Module Map E2E surface](/design/module-map/#e2e-test-surface).
