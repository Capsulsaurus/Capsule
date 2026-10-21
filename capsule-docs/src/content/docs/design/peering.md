---
title: Peering
description: Direct LAN device-to-device sync within a single user's own devices
status: draft
---

Peering is **device-to-device** sync within one user's own devices; server-to-server sharing is [Federation](/design/federation/). It is an **accelerator, never a replacement** for [server sync](/design/import/): it moves assets at LAN speed, lets devices converge while the server is unreachable, and does nothing if no peer is found. Implementation: `capsule-sdk::peering`.

## Peering Reuses, Not Reinvents

Peering adds **no new payload format and no new sync engine**. The unit of transfer is a delta-scoped [backup artifact](/design/backup-recovery/#backup-artifact), ingested through the **same restore path** as any backup. Peering owns only **discovery** and a **transport**.

## Trust Model

Unlike federation, both ends are the *same user's* devices, each with a DSK cross-signed into the [device directory](/design/cryptography/keys/#device-directory). Identity-trusted is **not** content-trusted: every received asset is re-verified (content hash, [STREAM tags](/design/cryptography/encryption/#stream-construction), [`verify_asset`](/design/cryptography/keys/#write-authorization)).

### Peer-Class Containment

Each device is its own [containment boundary](/design/threat-model/#damage-containment-layers):

- A received manifest behind the local `latest_provenance_hash` is quarantined ([Applying Received Data](#applying-received-data)).
- Any `sidecar_schema`, `crypto_suite_id` or `protocol_version` above the receiver's maximum is rejected at decode.
- A device removed from the directory cannot complete the handshake, and its previously cached state is treated as suspect.

## Discovery

Devices advertise over **mDNS** and accept **TCP**, **LAN-only** (no relay). The advertisement MUST NOT leak identity: an **opaque instance name**, rotated at least per boot and at most every 24 h, never a handle or device name. Same-user is established inside the channel. No answer means silent fallback to server sync.

The live mDNS responder is post-v1; v1 ships the rotating-advertisement logic behind the `Discovery` seam, tested in-process.

## Establishing the Channel

HTTP over **mutually authenticated TLS 1.3** with no CA. TLS authenticates the device key's **classical half**; the **hybrid** check that the key chains to the shared User IK (per the [signature scheme](/design/cryptography/primitives/#signature-scheme) and the device directory) runs above the channel, bound to the session, before any payload byte.

Peering assumes both devices are already provisioned. Adding or recovering a device is [Device Enrollment](/design/device-enrollment/) and [Backup and Recovery](/design/backup-recovery/#default-mechanisms).

## Determining the Delta

Each side offers its held content addresses and [sync cursor](/design/sync/#discovering-what-changed); the delta is the complement.

## What Moves Over the Wire

A delta-scoped backup artifact honoring the receiver's [Synchronization Scope](/design/sync/#synchronization-scope): metadata blobs and needed AMK versions always; originals and derivatives only up to the configured tier. Blobs the receiver already holds are skipped.

## Transfer Protocol

**Pull-only**, like [federation](/design/federation/#pull-only-federation): the device that is behind pulls and applies only what verifies. A peer may send an advisory [hint](/design/notifications/#vocabulary), which carries no authority and is never shown as an alert. Fetches use `GET` with `Range`, so transfers are resumable and idempotent.

## Applying Received Data

Ingest is the backup restore path. A manifest whose `prior_provenance_hash` is behind the local chain head ([stale-revival detection](/design/sync/#stale-revival-detection)) is **quarantined and surfaced** as "peer sent stale state"; a peer can never resurrect a locally tombstoned asset. Verification failures follow [soft-fail semantics](/design/federation/#soft-fail-semantics) and are recorded in the [audit trail](/design/cryptography/provenance/#provenance-of-library-modifications).

## Reconciliation with the Server

Peered assets carry their signed manifests, so the server later resolves them through normal [dedup and merge](/design/transfer/upload/#deduplication-and-merge); nothing is re-uploaded.

## Versioning

Two independent surfaces, both checked up front ([handshake](/design/threat-model/validation/#protocol-and-capability-negotiation)):

- **Transport protocol:** date-based `X-Capsule-Protocol`, its own version space separate from the server protocol. Mismatch ends the connection before any payload (`426`); no degraded mode.
- **Artifact format:** versioned by [Backup and Recovery](/design/backup-recovery/#backup-artifact); forward-jumping `crypto_suite_id` or `protocol_version` is rejected.

## Robustness

Range transfers resume; a vanished peer equals no peer; offline LAN convergence works; reordering a transfer cannot change state ([no order trust](/design/federation/#reconstructing-state-without-trusting-peers)).

## Validation

- **Advertisement (unit):** no handle or device name; rotates.
- **Handshake (unit):** same-IK succeeds; revoked device and foreign IK fail.
- **Delta (unit):** complement of two address sets.
- **Ingest (smoke):** A's artifact restores fully on B.
- **Stale revival (smoke):** pulling an older chain head quarantines.
- **Resume (smoke):** a severed transfer resumes without re-fetching.
- **LAN sync then server reconcile (E2E):** one case in [Module Map](/design/module-map/#e2e-test-surface).
