---
title: Clients
description: Native client priorities, what every client must validate, and the sandboxed decoder
status: draft
---

Capsule's clients are native per platform, with as little divergence as possible. The cross-platform logic — including the entire [`verify_asset`](/design/cryptography/keys/#write-authorization) chokepoint, the [import pipeline](/design/import/pipeline/), and the [library layout](/design/filesystem/client/) — lives in `capsule-core` and is consumed by every native client through `capsule-sdk`. Each native client's job is the surface above that: rendering, input, and platform integration.

The boundary this doc owns is **what every client must do** — the client-class duties that, if skipped, put the client in the *faulty* class (see [Threat Model — Client Class Taxonomy](/design/threat-model/#client-class-taxonomy)). Plus the sandboxed-decoder pattern, which is the largest remaining attack surface on the client.

The offline-first requirement set every native client must satisfy — the local-gallery FRs/NFRs, the gated Recently-Deleted/Hidden views, and the at-rest posture matrix — is owned by [Local Gallery](/design/local-gallery/).

## Design Priorities

- **Native.** Native implementations per platform ensure familiar usability and enable platform-specific optimizations.
- **Minimal divergence.** Heavy and complex logic is centralized in `capsule-core` and `capsule-sdk`; client-specific code is generally minimal and focused on display.

## Platform Limitations

Given the quantity of distinct native clients (each with its own platform-specific portion), certain features are limited to certain platforms — notably [auto sync](/design/import/download-sync/#auto-syncing) on platforms where the necessary APIs are not available.

**Status note.** Background upload and push-driven auto-sync (e.g. iOS `BGTaskScheduler`) are post-v1 (decision 2026-07-12); v1 sync and upload are foreground-initiated.

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

Centralizing the validation logic in `capsule-core` ensures each native client gets the same enforcement; the wrapper layer that issues UI surfaces for quarantine and protocol-mismatch errors is the platform-specific portion.
- **Run the [recovery verification cadence](/design/backup-recovery/#recovery-verification-cadence)** — and never persist the passphrase (or any derivative able to satisfy the check) to auto-pass it: a client that does so is buggy by definition, because the check exists to verify the *user* still holds the secret.

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
- **Forbidden-behavior tripwire (unit).** For each item in the [forbidden-behaviors checklist](/design/threat-model/schema-rules/#forbidden-client-behaviors) **that maps to a `capsule-core` API** — backdating into signed structures, stripping unknown sidecar fields, overwriting provenance, signing for an epoch the client does not hold — a unit test confirms the API panics or returns a structural error (so a buggy client cannot accidentally do the wrong thing). The auth-surface items (notably `revoke_all_sessions` without master-key proof) are server-enforced and tested in `capsule-api-auth`, not in core.

There is no client-only E2E case; the closest cross-module test is the upload-and-display round-trip used by the [Import](/design/import/) pipeline, which is bounded E2E in [Module Map](/design/module-map/#e2e-test-surface).
