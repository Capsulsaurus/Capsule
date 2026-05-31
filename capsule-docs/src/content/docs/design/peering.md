---
title: Peering
description: Direct LAN device-to-device sync within a single user's own devices
---

Peering is **device-to-device** sync within a single user's own devices. It is distinct from [Federation](/design/federation/), which is server-to-server sharing across *different* users.

Peering exists as an **accelerator, never a replacement** for normal [server synchronization](/design/import/). It earns its place in three situations:

- **LAN-speed transfer.** Two of a user's devices on the same network can move a freshly imported asset directly, instead of round-tripping every byte through the server and the internet.
- **Offline operation.** When the server is unreachable, devices on a shared LAN still converge. This satisfies the [offline/online divide](/design/principles/) — peering works fully offline.
- **Best-effort opportunism.** If no peer is found, peering simply does nothing and the device falls back to server sync. Nothing depends on it succeeding.

Peering is the one module here that lives entirely on the client. It is implemented in `capsule-sdk::peering` (discovery, channel, transfer) over `capsule-core::backup` (the artifact format it ingests). The three contract surfaces — mDNS descriptor, TLS handshake parameters, delta-fetch protocol — are the only new primitives peering introduces; everything else is borrowed.

## Peering Reuses, Not Reinvents

Peering deliberately introduces **no new payload format and no new sync engine** — the same discipline [Federation](/design/federation/#federation-reuses-existing-primitives) applies. The unit of transfer is a delta-scoped [backup artifact](/design/backup-recovery/#backup-artifact): a self-describing, versioned, encrypted, content-addressed blob that already exists for [Backup and Recovery](/design/backup-recovery/).

The receiving device ingests that artifact through the **same restore path** it would use for any backup. Peering therefore owns only two things of its own — a LAN **discovery** mechanism and a **transport**. Everything else (what an asset is, how it is encrypted, how it is verified, what "changed" means) is borrowed from designs that already exist and are already audited. Fewer moving parts means a smaller blast radius and far less code unique to peering.

## Trust Model

Federation assumes [a remote server is hostile](/design/federation/#threat-model). Peering does not: both endpoints are the *same user's* devices, each holding a hardware-bound DSK cross-signed into that user's [device directory](/design/cryptography/keys/#device-directory). A peer is accepted only after a mutual hybrid-signature check confirms both devices chain to the same User IK.

Identity-trusted is **not** content-trusted, however. A device can still be buggy, or compromised at the application layer above its hardware keys. So peering keeps Federation's posture toward *data*: every received asset is re-verified — its [ciphertext content hash](/design/cryptography/primitives/) recomputed, its [STREAM tags](/design/cryptography/encryption/#stream-construction) checked, its [asset manifest](/design/cryptography/provenance/#asset-manifest) run through the single [`verify_asset`](/design/cryptography/keys/#write-authorization) chokepoint. The channel authenticates *who* you are talking to; it never exempts *what* they send from validation.

### Peer-Class Containment

Even two of the same user's devices are separate failure-containment boundaries ([Threat Model — Damage Containment Layers](/design/threat-model/#damage-containment-layers)). A buggy $v_k$ device cannot overwrite a $v_{k+1}$ device's state via a stale-but-valid backup artifact, and a v_{k+1} device's writes are not retroactively applied to a v_k device's view of an older album. Specifically:

- Every received manifest is checked against the receiver's local `latest_provenance_hash` for that asset (see [Applying Received Data](#applying-received-data)) — a stale manifest is quarantined, not silently applied.
- Every received structure that announces a `sidecar_schema`, `crypto_suite_id`, or `protocol_version` above the receiver's max known is rejected at decode — the receiver refuses to interpret bytes it cannot validate. This is the client-side counterpart of the [server-side schema lockdown](/design/threat-model/schema-rules/#schema-evolution-and-field-grammar).
- Device-directory revocations are honored immediately: a device that has been removed from the user's directory cannot complete the TLS handshake (its certificate no longer chains to a current IK signature), and any prior cached state from that device is treated as suspect.

## Discovery

Discovery is the one genuinely new mechanism. Devices advertise a peering service over **mDNS** on the local network and accept connections over **TCP**.

Discovery is **LAN-only** — there is no relay, no internet-wide rendezvous. mDNS broadcasts are visible to every host on the segment, so the advertisement must not leak identity: a device advertises an **opaque, rotating service instance**, not `user@server.tld` or a device name. Whether two advertisements belong to the same user is established *inside* the encrypted channel (below), never from the broadcast itself.

If no peer answers, discovery fails silently and the device proceeds with ordinary server sync.

## Establishing the Channel

A peer connection is HTTP over a **mutually authenticated TLS 1.3** channel. The certificates presented are the **device keys themselves** — there is no CA. Each side verifies that the other's device certificate carries a valid hybrid signature chaining to the shared User IK, exactly as published in the [device directory](/design/cryptography/keys/#device-directory). The directory *is* the trust anchor; a device not in it cannot complete the handshake.

This doc covers sync between devices that are **already provisioned** — both already hold the account master key. Bootstrapping a brand-new device (handing it the master key for the first time) is **cross-device recovery** and is specified in [Device Enrollment](/design/device-enrollment/); peering does not re-document it.

## Determining the Delta

Before building an artifact, the two devices must agree on what is missing. Peering reuses the [sync cursor](/design/import/download-sync/#discovering-what-changed) model rather than inventing a diff: each side offers its set of held [ciphertext content addresses](/design/cryptography/primitives/) and its cursor, and the delta is the complement. "What changed" is already defined by the `/sync` feed — peering borrows that definition wholesale.

## What Moves Over the Wire

The transfer payload is a [backup artifact](/design/backup-recovery/#backup-artifact) scoped to the delta — backup artifacts are explicitly *"constructed from a list of assets, albums, and so on,"* so a delta-scoped one needs no special construction path.

Its contents honor the receiver's existing per-library [Synchronization Scope](/design/import/download-sync/#synchronization-scope) setting — there is no peering-specific knob:

- **Always included:** the encrypted metadata blobs and the AMK versions needed to decrypt the transferred assets. Without these the receiver cannot interpret anything.
- **Per scope:** original and derivative blobs are included only up to the receiver's configured tier (*metadata only* / *+ thumbnails* / *+ original*). Tiers above the setting are fetched lazily later, just as with server download.

Because every blob is content-addressed, dedup is free: the receiver skips any blob whose [content hash](/design/cryptography/primitives/) it already holds — the same lookup the `/blob/{hash}` download path performs against its local cache.

## Transfer Protocol

Peering is **pull-only**, mirroring [Federation](/design/federation/#pull-only-federation): the device that is behind initiates the pull and applies the result only after it verifies. A peer that has new content may send a lightweight **notification hint** — "new content exists" — over a separate low-trust channel to prompt a pull sooner; that hint never feeds the validation pipeline directly and carries no authority.

The artifact is fetched with HTTP `GET` and `Range` requests, which makes a transfer **resumable** across the flaky-by-nature LAN and **idempotent** — content-addressing turns a re-fetch of an already-held blob into a no-op. This is the same resumability the [upload](/design/import/upload-protocol/) and [download](/design/import/download-sync/#resumption-and-verification) paths rely on.

## Applying Received Data

A received artifact is ingested through the **backup restore path** — peering adds no separate deserialization. Restore already re-verifies every blob's [ciphertext content hash](/design/cryptography/primitives/), checks [STREAM tags](/design/cryptography/encryption/#stream-construction) on decrypt, and runs each asset manifest through [`verify_asset`](/design/cryptography/keys/#write-authorization).

Additionally, every received manifest's `prior_provenance_hash` is checked against the receiver's local `latest_provenance_hash` for that asset (see [Import — Stale-Revival Detection](/design/import/download-sync/#stale-revival-detection)). A peering pull cannot resurrect an asset the local device has tombstoned at a later provenance step — even if the artifact was honestly produced from an older state of the sending device. The stale entry is **quarantined and surfaced** as "peer sent stale state."

Failures follow Federation's [soft-fail semantics](/design/federation/#soft-fail-semantics): an asset that fails verification is **quarantined and surfaced** in the [provenance/audit trail](/design/cryptography/provenance/#provenance-of-library-modifications), never silently dropped and never silently accepted — so a bug can be told apart from an attack after the fact.

## Reconciliation with the Server

Peering does not fork a device's state away from the server. A peering-received asset arrives with its signed manifest intact, so when the server later sees the same asset — uploaded by whichever device the [upload policy](/design/import/download-sync/#synchronization-scope) assigns — it resolves through the existing [deduplication and merge](/design/import/upload-protocol/#deduplication-and-merge) path on the [content hash](/design/cryptography/primitives/). A device never re-uploads a blob the server already holds, and the two devices remain convergent with the server's view.

## Versioning

Peering has two independently versioned surfaces, both checked **once, up front**, crashing early on mismatch per [Principles](/design/principles/) and the universal [protocol handshake](/design/threat-model/validation/#protocol-and-capability-negotiation):

- The peering **transport protocol** — date-based (`YYYY-MM-DD`), exchanged via `X-Capsule-Protocol` at channel establishment. Mismatch terminates the TLS connection **before any payload byte is sent** — `426 Upgrade Required` in the channel's framing layer. There is no degraded-mode fallback; peering simply fails and the device proceeds to ordinary server sync.
- The **artifact format** — versioned by [Backup and Recovery](/design/backup-recovery/#backup-artifact), so a newer device can still ingest an artifact built by an older one. The artifact's `crypto_suite_id` and album `protocol_version` are validated against the receiver's max known on ingest; a forward-jumping value is rejected (refuse-by-default), never best-effort-parsed.

These two surfaces are independent: a device with up-to-date transport protocol may still receive an artifact format it does not implement (and vice versa). Both checks must pass before any bytes are applied to local state.

## Robustness

Peering's failure posture falls out of the designs it reuses:

- **Interruption.** `Range`-based transfers resume; nothing is re-sent unnecessarily.
- **Peer disappears.** A vanished peer is indistinguishable from "no peer found" — the device falls back to server sync. Peering is best-effort.
- **Offline.** With no server reachable, devices on a shared LAN still converge; the feature works solely offline.
- **No order trust.** Content-addressed, immutable blobs and signed manifests mean a peer cannot influence state by reordering a transfer — the same guarantee Federation states in [Reconstructing State Without Trusting Peers](/design/federation/#reconstructing-state-without-trusting-peers).

## Validation

- **mDNS opaque identifier (unit).** Generate an advertisement; assert it carries no user handle, no device name. Re-generate after the rotation interval; assert a new opaque identifier.
- **TLS mutual-auth handshake (unit).** Two device certificates chaining to the same IK — assert handshake succeeds. Replace one cert with a revoked-device cert — assert handshake fails. Replace one cert with a foreign-user IK cert — assert handshake fails.
- **Delta calculation (unit).** Two devices with overlapping but distinct content-address sets; assert the delta is the symmetric difference.
- **Artifact ingest (smoke).** Build a delta-scoped backup artifact on device A; feed to device B; assert restore path applies every asset; assert byte-equal `library.sqlite` rebuild on both sides.
- **Stale-revival quarantine on peer pull (smoke).** Device A holds an old manifest; device B holds a newer chain head; A pulls from B successfully; B pulls from A — assert quarantine, not silent overwrite.
- **Resume across LAN drop (smoke).** Start a large artifact transfer; sever the LAN; reconnect; assert Range-resumed transfer with no re-fetched bytes.

The cross-module case — full A→B LAN sync with both devices then reconciling with the server — is one bounded E2E case in [Module Map](/design/module-map/#e2e-test-surface).
