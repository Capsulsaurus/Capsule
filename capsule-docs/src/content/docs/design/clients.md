---
title: Clients
description: Platform tiers and design language, what every client must validate, and the sandboxed decoder
status: draft
---

Capsule's clients are native per platform, with as little divergence as possible. The cross-platform logic — including the entire [`verify_asset`](/design/cryptography/keys/#write-authorization) chokepoint, the [import pipeline](/design/import/pipeline/), and the [library layout](/design/filesystem/client/) — lives in `capsule-core` and is consumed by every native client through `capsule-sdk`. Each native client's job is the surface above that: rendering, input, and platform integration.

The boundary this doc owns is **what every client must do** — the client-class duties that, if skipped, put the client in the *faulty* class (see [Threat Model — Client Class Taxonomy](/design/threat-model/#client-class-taxonomy)). Plus the sandboxed-decoder pattern, which is the largest remaining attack surface on the client.

The offline-first requirement set every native client must satisfy — the local-gallery FRs/NFRs, the gated Recently-Deleted/Hidden views, and the at-rest posture matrix — is owned by [Local Gallery](/design/local-gallery/).

## Design Priorities

- **Native.** Native implementations per platform ensure familiar usability and enable platform-specific optimizations.
- **Minimal divergence.** Heavy and complex logic is centralized in `capsule-core` and `capsule-sdk`; client-specific code is generally minimal and focused on display.

### Platform Tiers

Every native client is intended to end up **fully featured and platform-integrated** — Windows and Linux included. The tiers below are the order in which they get there, not a permanent hierarchy, and not a statement that a lower tier gets a lesser product. A client is *first-class* when it has full feature parity and idiomatic platform integration; the tier says when that is expected, not whether it is owed.

| Tier | Platforms | Meaning |
| --- | --- | --- |
| 1 — stabilizing now | iOS, Android (phone) | The two the product is designed against. Every feature lands here first. |
| 2 — next | iPadOS, Android tablet | Large-screen adaptations of tier 1, not separate products. |
| 3 — after | macOS | Desktop-idiomatic; shares its views with iPadOS. |
| Deferred | Windows, Linux | Real targets, not started. |
| Not on the ladder | Web | Permanently read-only; see [Web Client](#web-client). |

The phone tiers lead because a photo library is overwhelmingly captured and consumed on a phone. Tablets and desktops earn their place by being *better* at the things phones are bad at — culling a thousand frames, a wide timeline, a real keyboard — which is work that only pays off once the feature it operates on exists.

### What Clients Share

Sharing is decided by what actually differs between two platforms, not by which company makes them:

- **iPadOS and macOS share views.** Both present a regular-width split view, both have a pointer and a keyboard, and both address the same navigation graph. A screen written for one renders on the other; divergence is confined to the shell around it (a menu bar, a Settings scene, detached windows) rather than to the screens themselves.
- **iOS and Android share layouts and feature sets, but not design language.** Same information architecture, same navigation graph, same screens in the same order. What differs is the vocabulary they are drawn in — see [Design Language](#design-language). A feature present on one phone and absent on the other is a bug in the plan, not a platform difference.
- **Apple-specific logic is shared across iOS, iPadOS and macOS.** One workspace, one route enum, one router, three shells chosen by size class rather than by `#if os(...)`. The same is intended for Android phone and tablet.

The rule this produces: **a difference between two clients has to be justified by a difference in the platform.** Screen real estate, input device, and platform convention are justifications. Which client was written first is not.

### Design Language

Each client is drawn in its own platform's vocabulary, over a shared skeleton. The skeleton — what screens exist, what they contain, how they nest — is common; the material is not. On Apple platforms that means stock controls, system materials, SF Symbols and the HIG; on Android it means Material. **Neither client emulates the other**, and neither invents a third, house style: an app that looks equally foreign everywhere is the failure mode this rule exists to prevent.

Bespoke components are reserved for concepts the host platform has no analogue for — seal state, verification verdicts, quarantine surfaces, the degrade ladder, the smart-album predicate builder, ceremony progress. Those are drawn once per platform in that platform's idiom, not copied between them.

Because the *strings* belong to the shared skeleton rather than to a platform, they are namespaced by surface rather than by client: a key naming a product concept present on both phones carries no platform prefix, and only genuinely platform-only chrome (a macOS menu bar) gets one. See [i18n](/design/i18n/).

### Web Client

The web client is not a tier on the ladder above, because it is not trying to become a native client. It is **permanently read-only**, in two modes:

- **Authenticated viewing** — a signed-in user browsing their own library.
- **Public links** — [share links](/design/share-links/) for viewing, and [upload links](/design/web-upload/) for guest drops.

It never writes library state, never enrolls a device, and never uploads outside a guest drop. This is a consequence of the [key hierarchy](/design/cryptography/keys/), not a product decision that could be reversed by adding screens: a browser holds neither the hardware-bound device keys nor the write-tier keys those operations require. Guest drops are the sole exception and are quarantined by construction — the guest is keyless and the drop is sealed to the recipient, which is why it needs no write authority.

Two further properties separate it from every native client:

- **Server-owned.** It ships with the server and is updated by the server administrator. A user does not choose its version, and there is no app store between the two.
- **Version-locked to its server.** Native clients must be lenient about which server versions they accept — a user's phone and their server update on unrelated schedules, which is why [protocol evolution is additive](/design/versioning/). The web client is served *by* the server it talks to, so that skew cannot arise and it needs no such leniency. It is the one client permitted to assume its peer's exact version.

This section owns the web client's **scope** — read-only, server-owned, version-locked. It does not own the guest-drop path: the keyless *web uploader* class, the sealed drop object, and the adoption transition are owned by [Web Upload](/design/web-upload/), and the public view-only link surface by [Share Links](/design/share-links/).

**Status note — the Apple client is built against ports, not against the SDK (2026-08-22).** It was written while `capsule-sdk`'s FFI verbs did not yet exist, because waiting for them would have left the client unbuilt at the moment they landed — the worst sequencing for the surface with the most design work and the slowest review loop. Those verbs have since landed (`S-P1`), so the swap described below is now schedulable rather than hypothetical. So the Apple client's data seam is a set of `async`/`Sendable` protocols with an in-memory adapter behind them, and every screen is written and tested against that. The domain types those protocols carry are shaped as structural matches for the uniffi records they will be generated from, so the swap is a constructor change in one composition root. **This does not relax any duty below.** `verify_asset` quarantine states, forward-version refusal, and the unreadable-on-this-device surface are all reachable and tested in the mocked client — a client that cannot *show* a quarantine is not a client that can be trusted to enforce one. The slice list is lane U in the repo-root `SLICES.md`.

## Platform Limitations

Given the quantity of distinct native clients (each with its own platform-specific portion), certain features are limited to certain platforms — notably [auto sync](/design/import/download-sync/#auto-syncing) on platforms where the necessary APIs are not available.

**Status note.** Background upload and OS-scheduled auto-sync (the iOS `BGTaskScheduler` family, and the equivalent elsewhere) are post-v1 (decision 2026-07-12); v1 sync and upload are foreground-initiated. Server-driven wake is a separate question with its own answer — see [Notifications — Tier 1](/design/notifications/#tier-1--wake), which is also post-v1 and rules out APNs and FCM permanently.

User-facing **alerts** are not deferred: they are v1, entirely local, and owned by [Notifications](/design/notifications/). A client that cannot get a background window can still warn its user, because deadline-driven alerts are pre-armed on the OS timer rather than evaluated when the app happens to run.

## Client Validation Duties

Clients are not trusted to enforce their own correctness — but they **are** responsible for **refusing to apply** state they cannot validate. The full client-side validation checklist is owned by [Threat Model — Client-Side Validation Invariants](/design/threat-model/validation/#client-side-validation-invariants); the duties are summarized here so client implementations have a single in-doc reference for what they must do:

- **Run [`verify_asset`](/design/cryptography/keys/#write-authorization)** on every received asset manifest. Quarantine on failure; never silent-drop, never silent-accept. This is *the* chokepoint every client must route through — it is implemented once in `capsule-core::crypto` and called by every receiving path (sync, federation, peering, backup-restore).
- **Refuse forward-version writes.** Reject any incoming `sidecar_schema`, `crypto_suite_id`, or `protocol_version` above the client's max known. Reading is allowed only in read-only mode if explicitly opted into.
- **Enforce the protocol handshake.** Send `X-Capsule-Protocol` on every request; honor `426 Upgrade Required` by stopping the request, never by silently downgrading.
- **Check the provenance chain.** Maintain a local `latest_provenance_hash` per asset; refuse to apply a manifest whose `prior_provenance_hash` is behind it. See [Import — Stale-Revival Detection](/design/import/download-sync/#stale-revival-detection).
- **Reject unknown closed-enum values.** *Every* closed enum rejects unknown values as structural errors, never "future to ignore" — the blanket per-`protocol_version` rule is owned by [Threat Model — Schema Rules](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar), and each value set by its owner doc (among them `action`, `content_type`, `gps.source`, `key_mode`, `DerivativeManifest.role`/`format`, `StackMembership.stack_type`/`role`). This list is illustrative; the rule is total.
- **Preserve unknown CBOR keys within a known schema** (Postel's Law) but never act on them.
- **Decode remote-origin asset bytes only in the [Sandboxed Decoder](#sandboxed-decoder).**
- **Honor the [forbidden behaviors checklist](/design/threat-model/schema-rules/#forbidden-client-behaviors).** A client that backdates timestamps, strips unknown sidecar fields, overwrites provenance, signs for an epoch it does not hold, or invokes `revoke_all_sessions` without master-key proof is *buggy by definition*.
- **Run the [recovery verification cadence](/design/backup-recovery/#recovery-verification-cadence)** — and never persist the passphrase (or any derivative able to satisfy the check) to auto-pass it: a client that does so is buggy by definition, because the check exists to verify the *user* still holds the secret.

Centralizing the validation logic in `capsule-core` ensures each native client gets the same enforcement; the wrapper layer that issues UI surfaces for quarantine and protocol-mismatch errors is the platform-specific portion.

## Reading State From a Newer Client

A client routinely encounters state a *newer* client wrote: unknown CBOR keys inside a known schema (always preserved per Postel's Law), or — under an explicit read-only opt-in — a sidecar whose `sidecar_schema` exceeds the reader's max known. The duty is to render what it can without ever destroying what it cannot interpret:

- **Render the known, surface the unknown.** The client displays every field it understands and shows a **non-destructive indicator** on the affected asset/album — "Created with a newer version of Capsule; some details may not be shown or editable here" — rather than failing, hiding, or quarantining the asset.
- **Never strip, never rewrite.** Unknown CBOR keys and forward-schema sidecars are strictly read-only: the client never writes back a structure it cannot fully represent, because doing so would strip the extension and invalidate the signature — a [forbidden behavior](/design/threat-model/schema-rules/#forbidden-client-behaviors). Editing such an asset is disabled behind the same indicator, pointing the user to update.
- **Writes still fail closed.** Reading newer state is best-effort and read-only; *writing* under a `protocol_version`, `crypto_suite_id`, or `sidecar_schema` the client does not implement remains rejected at the [handshake](/design/threat-model/validation/#protocol-and-capability-negotiation). Tolerant reads, fail-closed writes — the [tightened Postel's Law](/design/principles/#postels-law-asymmetric).

This is the resolution of the former "new client UI surface" question: forward-written state is legible and safe, never silently dropped and never destructively rewritten.

## Sandboxed Decoder

**Status: contract fixed, platform implementations post-v1** (decision 2026-07-12). Until a platform's sandbox lands, a client decoding remote-origin bytes in-process is running a documented deviation of this contract — the isolation requirement stands and is tracked in the post-v1 register (`SLICES.md`).

Capsule's server never holds plaintext, so server-side image/video decoding is impossible by design. **Decoding happens on the client**, and the decode path is the largest remaining attack surface — image-format CVEs (libjpeg, libwebp, libheif, libavif have all shipped exploits in recent years) reach the client directly with attacker-controlled bytes.

The defense is structural isolation:

- **Every remote-origin asset is decoded in a separate OS process or a WASM sandbox** that has no filesystem write access, no network access, and no shared memory with the host app process. The isolation primitive differs per platform — an XPC service / app extension on Apple platforms, an `isolatedProcess` service on Android, a privilege-dropped subprocess on desktop, a Worker-hosted WASM sandbox in the browser — and these invariants are the contract each mechanism must meet; a platform that cannot meet one documents the deviation in its client rather than silently weakening the boundary.
- The sandbox communicates with the host via a narrow IPC channel that exchanges only the produced pixel buffer (or an error code) — not arbitrary structured data.
- **The sandbox is allowed to crash.** A decoder CVE that triggers a segfault kills the sandbox, not the app. The host process logs the crash, surfaces "asset failed to decode," and continues. The sandbox is restarted on the next decode request.
- **Local-origin assets** (this device was the uploader and the bytes have never left local storage) bypass the sandbox at the user's option — they have not crossed a trust boundary. By default the sandbox is still used uniformly, because the modest perf cost is worth the categorical guarantee.
- A media file that still fails to decode after a small fixed retry budget (default 3 attempts, to absorb a transient sandbox crash) is flagged in the UI as "unreadable on this device" rather than removed from the library — the bytes are preserved (per [Filesystem — Repair](/design/filesystem/maintenance/#repair)) for inspection on another device.

This is the canonical declaration of the sandbox; [Federation — Security Against Malicious Files](/design/federation/#security-against-malicious-files) references it for the federated-asset case, and [Backup & Recovery — Backup Verification](/design/backup-recovery/#backup-verification) references it for dry-run decode sanity checks.

## Test and Performance Tooling

This section owns the per-platform test-framework and performance-tooling pins (the cross-platform library pins live in [Dependencies](/design/dependencies/)).

- **Apple platforms:** **swift-testing** (`@Suite`/`@Test`) is the sole unit/smoke framework. XCTest is sanctioned **only** inside XCUITest UI-automation bundles, where no swift-testing analogue exists. The `capsule-core-swift` harness's existing XCTest smoke migrates (slice `S-F7` in the repo-root `SLICES.md`); new XCTest unit tests are not accepted.
- **Apple performance work:** **Instruments** (Time Profiler, Allocations, Leaks, Network, Core Animation) for interactive profiling, and **MetricKit** for field metrics (launch time, hangs, memory, disk writes) — no third-party APM runs on-device.
- **Kotlin/Android:** JUnit 5 (Jupiter) is the current harness for the `capsule-core-kotlin` bindings; the canonical pin for the Compose app is recorded here when its test harness stabilizes, under the same one-framework-per-platform principle.
- **Web:** the test runner rides the toolchain pinned in [Dependencies — Web](/design/dependencies/#web) (bun's built-in runner today).

## Validation

The validation duties above translate directly to test surface. Most live in `capsule-core` (so they apply uniformly to every client); the per-platform pieces are the sandbox harness.

- **`verify_asset` per-receiver-path (unit).** Every receiver code path (sync entry, federation pull, peering artifact, restore) routes through `verify_asset`; assertion test confirms the same chokepoint is used, not a divergent implementation.
- **Forward-version rejection (unit).** Per-validation-duty unit test: synthesize an input whose declared version exceeds the client's max; assert *write* refusal.
- **Forward-state read surface (unit).** Present a sidecar with unknown CBOR keys and (opt-in) a higher `sidecar_schema`; assert known fields render, the non-destructive "newer version" indicator shows, editing is disabled, and any write-back attempt is refused *without* stripping the unknown keys.
- **Sandbox crash isolation (smoke per platform).** Feed the sandbox a known-CVE corpus; assert the host process survives every crash; assert the asset is surfaced as "unreadable on this device" and not removed from the library.
- **Sandbox boundary (smoke per platform).** Assert the sandbox cannot read the parent process's filesystem, open network sockets, or write outside its scratch area. Per-platform fixtures verify each restriction.
- **Forbidden-behavior tripwire (unit).** For each item in the [forbidden-behaviors checklist](/design/threat-model/schema-rules/#forbidden-client-behaviors) **that maps to a `capsule-core` API** — backdating into signed structures, stripping unknown sidecar fields, overwriting provenance, signing for an epoch the client does not hold — a unit test confirms the API panics or returns a structural error (so a buggy client cannot accidentally do the wrong thing). The auth-surface items (notably `revoke_all_sessions` without master-key proof) are server-enforced and tested in `capsule-server::auth`, not in core.

There is no client-only E2E case; the closest cross-module test is the upload-and-display round-trip used by the [Import](/design/import/) pipeline, which is bounded E2E in [Module Map](/design/module-map/#e2e-test-surface).
