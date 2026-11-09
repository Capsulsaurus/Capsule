---
title: Clients
description: Platform tiers and design language, what every client must validate, and the sandboxed decoder
status: draft
---

Clients are native per platform. Cross-platform logic ([`verify_asset`](/design/cryptography/keys/#write-authorization), the [import pipeline](/design/import/pipeline/), the [library layout](/design/filesystem/client/)) lives in `capsule-core`, reached through `capsule-sdk`; a client owns rendering, input and platform integration. This doc owns what every client MUST do, since skipping it makes the client [*faulty*](/design/threat-model/#client-class-taxonomy), and the sandboxed decoder. [](/design/local-gallery/) owns the offline-first requirements.

## Design Priorities

- **Native** per platform, for familiar usability and platform optimisation: SwiftUI on Apple platforms and Jetpack Compose on Android, each over the shared Rust core through its UniFFI bindings ([ADR-0005](https://github.com/Capsulsaurus/Capsule/blob/master/adr/0005-one-uniffi-namespace-pair-reaches-the-apps.md)). No cross-platform UI framework sits between Rust and a client.
- **Minimal divergence.** Heavy logic is centralised in `capsule-core` / `capsule-sdk`, so client code is mostly display.

### Platform Tiers

Every native client is meant to end up fully featured and platform-integrated; tiers are an ordering, not a hierarchy.

| Tier | Platforms | Meaning |
| --- | --- | --- |
| 1 — stabilizing now | iOS, Android (phone) | Every feature lands here first. |
| 2 — next | iPadOS, Android tablet | Large-screen adaptations of tier 1. |
| 3 — after | macOS | Shares its views with iPadOS. |
| Deferred | Windows, Linux | Real targets, not started. |
| Not on the ladder | Web | Permanently read-only; see [](#web-client). |

### What Clients Share

- **iPadOS and macOS share views**; divergence is confined to the shell (menu bar, Settings scene, windows).
- **iOS and Android share layouts and feature sets, not design language.** A feature on one phone and not the other is a plan bug.
- **Apple logic is shared** across iOS, iPadOS and macOS: one route enum, one router, and shells chosen by size class, not `#if os(...)`. Android phone and tablet likewise.

A difference between two clients MUST be justified by a difference in the platform (screen, input, convention), never by which was written first.

### Design Language

The skeleton (which screens exist and how they nest) is shared and the material is per-platform: HIG, stock controls and SF Symbols on Apple, Material on Android. Neither emulates the other, and there is no third house style. Bespoke components exist only for concepts without a platform analogue (seal state, verification verdicts, quarantine, the degrade ladder, the smart-album predicate builder, ceremony progress), drawn in each platform's idiom. Strings are keyed by surface, not client ([](/design/i18n/)), and layout is direction-agnostic and expansion-tolerant on every client ([i18n — Layout](/design/i18n/#layout)). [](/development/visual-language/) owns the one brand token a native client adopts, its accent.

### Responsiveness

Capsule MUST feel at least as fast as the platform's own photo app (Photos on Apple, the Gallery or Google Photos app on Android) on the same device, at every library size.

- **Instant on press.** Every input produces a visible response in the next frame (≤16 ms at 60 Hz): a pressed state, the destination screen, or an in-place change. Nothing waits for a spinner first.
- **Local first.** Navigation and edits (labels, albums, favourites, time and location fixes) apply optimistically from local state within 100 ms. The network write follows and never blocks the screen, and a rejected write rolls back visibly with the reason.
- **No press waits on the network.** Work that needs the server (restore, share, sync) starts immediately with visible progress and can be left; the UI never blocks on a request.
- **The main thread is for drawing.** Decoding, hashing, crypto and database queries run off the UI thread, and a frame never waits on them.
- **Measured, not asserted.** A parity benchmark runs each tier-1 client and the platform photo app on a 100,000-asset library on the same physical device. It measures cold start to first thumbnail, scroll frame rate and dropped frames, full-resolution open time, and input-to-frame latency.
  - Both apps are timed by the same harness calls, so its overhead cancels out. Input-to-frame latency is the exception: a harness outside the app cannot see its frames, so the client marks each tap itself, from delivery until the answering frame is drawn. The platform app marks nothing, so that measure is reported as not compared.
  - Each device has a recorded baseline of the client's own numbers. A regression past it fails the run, and a baselined measure that stops being reported counts as a regression. Parity with the platform app is reported beside it rather than gating, because the platform app changes under the device's own updates while the baseline changes only when re-recorded.
  - Results recorded on another device, platform or library size are refused, not compared. Emulators and simulators never stand in for a bench device.
  - The harnesses are Macrobenchmark on Android and XCTest metrics on Apple, both driven by `mise run bench-apps`. They need a prepared device, so the run is a manual workflow on self-hosted bench runners (`bench-apps.yml`), not a gate on every pull request. In production the Android client logs JankStats frame summaries, so the field reports the same jank signal the bench measures.

The TUI follows the same frame bound for input response ([](/development/testing/)).

### Web Client

**Permanently read-only**: authenticated viewing, public [share links](/design/share-links/) and guest [upload links](/design/web-upload/). It never writes library state or enrolls a device, because a browser holds neither hardware-bound device keys nor write-tier keys ([key hierarchy](/design/cryptography/keys/)). Guest drops are keyless and sealed to the recipient, so they need no write authority; [](/design/web-upload/) owns that path.

A web session opens through either door the server offers: a password or, where `server-info` publishes `auth.oidc`, the identity provider's authorization-code flow. The browser binds itself to that ceremony with an S256 `binding_challenge` whose secret stays in the tab's `sessionStorage` until the provider redirects back to `/login/oidc-callback`, which the deployment registers as its `OIDC_REDIRECT_URL`. Either door ends in the same session and second-factor step, and neither grants anything a read-only client lacks.

It is **server-owned** (shipped and updated with the server) and **version-locked** to it, the one client allowed to assume its peer's exact version. Native clients MUST stay lenient across server versions, which is why [protocol evolution is additive](/design/versioning/).

A **progressive web app is out of scope, permanently**: no web app manifest, service worker, offline mode or install prompt. Offline and installed use is what the native clients are for, and a cached web client would outlive its server's version, breaking the version lock.

## Platform Limitations

Some features are platform-limited, notably [auto sync](/design/sync/#auto-syncing). Background upload and OS-scheduled sync are post-v1, so v1 sync is foreground-initiated. Server-driven wake is post-v1 and never uses APNs/FCM ([](/design/notifications/#tier-1--wake)). Local, pre-armed alerts are v1 ([](/design/notifications/)).

## Client Validation Duties

Clients MUST refuse to apply state they cannot validate. The full checklist is [](/design/threat-model/validation/#client-side-validation-invariants); in summary:

- **Run [`verify_asset`](/design/cryptography/keys/#write-authorization)** on every received manifest, on every receiving path (sync, federation, peering, restore). Quarantine on failure; never silently drop or accept.
- **Refuse forward-version writes**: any `sidecar_schema`, `crypto_suite_id` or `protocol_version` above the client's max.
- **Enforce the handshake**: send `X-Capsule-Protocol` on every request; on `426` stop, never downgrade.
- **Check the provenance chain**: refuse a manifest whose `prior_provenance_hash` is behind the local `latest_provenance_hash` ([](/design/sync/#stale-revival-detection)).
- **Reject unknown values of every closed enum** as structural errors ([Schema Rules](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar)).
- **Preserve unknown CBOR keys** within a known schema, but never act on them.
- **Decode remote-origin bytes only in the [](#sandboxed-decoder).**
- **Honour the [forbidden behaviours](/design/threat-model/schema-rules/#forbidden-client-behaviors)**: backdating, stripping unknown fields, overwriting provenance, signing for an unheld epoch, `revoke_all_sessions` without master-key proof.
- **Run the [recovery verification cadence](/design/backup-recovery/#recovery-verification-cadence)** and never persist the passphrase or anything that can satisfy the check.

## Reading State From a Newer Client

Tolerant reads, fail-closed writes ([Postel's Law, asymmetric](/design/principles/#postels-law-asymmetric)):

- **Render the known, surface the unknown.** Show every understood field plus a non-destructive "created with a newer version" indicator; do not fail, hide or quarantine. A higher `sidecar_schema` is readable only under explicit read-only opt-in.
- **Never strip or rewrite** a structure the client cannot fully represent; editing is disabled behind the indicator.
- **Writes still fail closed** at the [handshake](/design/threat-model/validation/#protocol-and-capability-negotiation).

## Sandboxed Decoder

The contract is fixed. Platform implementations are post-v1, and until then in-process decoding of remote-origin bytes is a documented deviation.

The server never holds plaintext, so decoding, the client's largest attack surface, happens on the client. Invariants:

- Every remote-origin asset is decoded in a separate process or WASM sandbox with **no filesystem write, no network and no shared memory** with the host: XPC service / extension on Apple, `isolatedProcess` on Android, a privilege-dropped subprocess on desktop, Worker WASM in the browser. A platform that cannot meet one documents the deviation.
- IPC carries only the pixel buffer or an error code.
- **The sandbox may crash.** The host logs it, reports a decode failure and restarts the sandbox on the next request.
- Local-origin bytes may bypass the sandbox by user option; the default is to sandbox everything.
- After a small retry budget (default 3), the asset is shown as "unreadable on this device" and **kept**, never removed ([Filesystem — Repair](/design/filesystem/maintenance/#repair)).

## Test and Performance Tooling

Per-platform pins; library pins carry their rationale beside the pin in each manifest.

- **Apple:** swift-testing is the sole unit/smoke framework, with XCTest only in XCUITest bundles. Profiling uses Instruments and field metrics MetricKit, with no third-party on-device APM.
- **Kotlin/Android:** JUnit 5.
- **Web:** bun's built-in runner.

## Validation

- **`verify_asset` chokepoint (unit).** Every receiver path routes through the one implementation.
- **Forward-version (unit).** Versions above max refuse writes; unknown keys and an opt-in higher schema render with the indicator, editing disabled, nothing stripped.
- **Sandbox crash and boundary (smoke, per platform).** A CVE corpus never kills the host; the asset becomes "unreadable on this device"; the sandbox cannot read host files, open sockets or write outside scratch.
- **Forbidden-behaviour tripwire (unit).** Each forbidden behaviour reachable through a `capsule-core` API returns a structural error; auth-surface items are tested server-side.
