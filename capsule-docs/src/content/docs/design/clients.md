---
title: Clients for Capsule
description: An overview of the core architectural decisions for clients in Capsule.
---

This document outlines the core architectural decisions for clients in Capsule, including the rationale behind them and how they contribute to the overall design of the system.

## Design Priorities

- **Native:** We prioritize native implementations for each platform to ensure familiar usability and enable platform-specific optimizations.
- **Minimal divergence:** While we carefully version everything where applicable and minimize data that acts as sources of truth, we heavily centralize all the heavy and complex logic in `capsule-core` and `capsule-sdk`. Any client-specific logic is generally minimal and focused on display.

## Platform Limitations

Given the quantity of distinct native clients (each having distinct portions of platform-specific logic), certain features are limited to certain platforms.

## Client Validation Duties

Clients are not trusted to enforce their own correctness — but they are responsible for **refusing to apply** state they cannot validate. The full client-side validation checklist is owned by [Threat Model — Client-Side Validation Invariants](/design/threat-model/#client-side-validation-invariants); the duties are summarized here so client implementations have a single in-doc reference for what they must do:

- **Run [`verify_asset`](/design/cryptography/#write-authorization)** on every received asset manifest. Quarantine on failure; never silent-drop, never silent-accept.
- **Refuse forward-version writes.** Reject any incoming `sidecar_schema`, `crypto_suite_id`, or `protocol_version` above the client's max known. Reading is allowed only in read-only mode if explicitly opted into.
- **Enforce the protocol handshake.** Send `X-Capsule-Protocol` on every request; honor `426 Upgrade Required` by stopping the request, never by silently downgrading.
- **Check the provenance chain.** Maintain a local `latest_provenance_hash` per asset; refuse to apply a manifest whose `prior_provenance_hash` is behind it. See [Import & Sync — Stale-Revival Detection](/design/import-synchronization/#stale-revival-detection).
- **Reject unknown closed-enum values.** `action`, `content_type`, `DerivativeManifest.role`, and `gps.source` are closed per protocol version; unknown values are structural errors, not "future to ignore."
- **Preserve unknown CBOR keys within a known schema** (Postel's Law) but never act on them.
- **Decode remote-origin asset bytes only in the [Sandboxed Decoder](#sandboxed-decoder).**
- **Never invoke `revoke_all_sessions` without master-key proof.** A pure session-token revoke-all is a [forbidden client behavior](/design/threat-model/#forbidden-client-behaviors).
- **Honor the [forbidden behaviors checklist](/design/threat-model/#forbidden-client-behaviors).** A client that backdates timestamps, strips unknown sidecar fields, overwrites provenance, or signs for an epoch it does not hold is *buggy by definition*.

Centralizing the validation logic in `capsule-core` (per [Design Priorities](#design-priorities)) ensures each native client gets the same enforcement; the wrapper layer that issues UI surfaces for quarantine and protocol-mismatch errors is the platform-specific portion.

## Sandboxed Decoder

Capsule's server never holds plaintext, so server-side image/video decoding is impossible by design. **Decoding happens on the client**, and the decode path is the largest remaining attack surface — image-format CVEs (libjpeg, libwebp, libheif, libavif have all shipped exploits in recent years) reach the client directly with attacker-controlled bytes.

The defense is structural isolation:

- **Every remote-origin asset is decoded in a separate OS process or a WASM sandbox** that has no filesystem write access, no network access, and no shared memory with the host app process.
- The sandbox communicates with the host via a narrow IPC channel that exchanges only the produced pixel buffer (or an error code) — not arbitrary structured data.
- **The sandbox is allowed to crash.** A decoder CVE that triggers a segfault kills the sandbox, not the app. The host process logs the crash, surfaces "asset failed to decode," and continues. The sandbox is restarted on the next decode request.
- **Local-origin assets** (this device was the uploader and the bytes have never left local storage) bypass the sandbox at the user's option — they have not crossed a trust boundary. By default the sandbox is still used uniformly, because the modest perf cost is worth the categorical guarantee.
- A media file that fails to decode after N retries in the sandbox is flagged in the UI as "unreadable on this device" rather than removed from the library — the bytes are preserved (per the recovery-first principle in [Filesystem](/design/filesystem/#repair)) for inspection on another device.

This is the canonical declaration of the sandbox; [Federation — Security Against Malicious Files](/design/federation/#security-against-malicious-files) references it for the federated-asset case, and [Backup & Recovery — Backup Verification](/design/backup-recovery/#backup-verification) references it for dry-run decode sanity checks.

## Additional Comments

- Compose Multiplatform was heavily considered initially for cross-platform logic but since most format processing is Rust and Kotlin/Native continues to have multiple limitations, we decided to stick to Rust-first approach.
