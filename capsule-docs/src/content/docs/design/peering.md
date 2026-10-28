---
title: Peering
description: Direct device-to-device sync and upload delegation between one user's own devices
status: draft
---

Peering connects **one user's own devices** directly; server-to-server sharing is
[Federation](/design/federation/). It has two uses:

| Use | What moves | Who ends up holding it |
| --- | --- | --- |
| **Sync** | A delta of what one device has and the other lacks | Both devices |
| **Delegation** | Sealed upload bundles a device has not yet uploaded | The delegate, until the server issues custody |

Peering is an **accelerator, never a replacement** for the server: it moves bytes at LAN speed
and lets devices converge while the server is unreachable, but a copy on another device is
**never custody**. Only a server custody receipt releases a local original
([invariant 7](/design/versioning/#system-invariants)). Implementation: `capsule-sdk::peering`.

## Payloads

Peering adds no payload format and no sync engine.

- **Sync** moves a delta-scoped [backup artifact](/design/backup-recovery/#backup-artifact),
  ingested through the backup restore path.
- **Delegation** moves upload bundles exactly as the [transfer protocol](/design/transfer/upload/#what-gets-uploaded)
  defines them: ciphertext blobs plus the signed manifest, sealed and signed by the originating
  device before they leave it.

Peering owns only discovery, the channel and the hand-off rules below.

## Trust Model

Both ends hold device keys cross-signed into the same
[device directory](/design/cryptography/keys/#device-directory). Identity-trusted is **not**
content-trusted: everything received is re-verified (content hash,
[STREAM tags](/design/cryptography/encryption/#stream-construction),
[`verify_asset`](/design/cryptography/keys/#write-authorization)), and a peer can at worst
withhold or delay, never alter, what it carries.

- **Directory anti-rollback.** Each device persists the highest `directory_version` it has
  verified. A peer presenting a lower version is refused, so a stale directory cannot readmit a
  revoked device.
- **Revocation is checked on every request**, not only at the handshake: a device removed from
  the directory mid-session is cut off at its next request, and everything it delivered after
  its revocation took effect is quarantined.
- **Each device is its own containment boundary.** A manifest behind the local chain head is
  quarantined; any `sidecar_schema`, `crypto_suite_id` or `protocol_version` above the
  receiver's maximum is rejected at decode.

## Discovery

Devices advertise over **mDNS** and accept **TCP**, **LAN-only**, with no relay. The
advertisement MUST NOT identify the user or device: an **opaque instance name**, rotated at
every boot and at least every 24 h, never a handle, device name or model. Same-user is
established inside the channel; no answer means silent fallback to the server. Handshake
attempts are rate-limited per source address, so the LAN cannot probe a device for membership.

The limiter tracks a bounded number of source addresses. A new source that finds the table full
of live ones **evicts the least recently active source**; it is never refused. The limiter keys
on the **full address**, not a prefix: on a LAN every IPv6 peer shares the link's /64 (and every
link-local peer shares `fe80::/64`), so keying by prefix would let one host exhaust the budget of
every peer on the link. The trade-off is that a host holding more addresses than the table
tracks can flush the oldest entries, resetting their budgets. That weakens the rate limit only
for a host that already gets a fresh budget with every new address it uses, and it never locks
a peer out.

The live mDNS responder ships behind the `Discovery` seam, tested in process.

## Establishing the Channel

A framed request/response exchange over **mutually authenticated TLS 1.3** with no CA. TLS
authenticates the device key's **classical half**; the **hybrid** proof that the key chains to
the shared User IK (per the
[signature scheme](/design/cryptography/primitives/#signature-scheme)) is bound to the TLS
exporter and verified before any payload byte. Both devices must already be enrolled
([Device Enrollment](/design/device-enrollment/)). The proof also covers the sender's role,
dialer or listener, so the two hellos of one session sign different messages and a hello
reflected back at its sender never verifies. A hello naming the receiving device's own id is
refused outright: a device never peers with itself.

Every payload byte travels inside that verified TLS session; there is no plaintext path. Each
frame's length is checked against a fixed ceiling before it is read, every read and write is
bounded in time, a ranged pull must sustain a minimum transfer rate, and every decision is made against the identity the handshake verified, never
against a device id a frame claims. The serving device lists what it offers, then authorizes
**each request** against the directory it pins at that moment, so a peer revoked mid-session is
refused at its next request. Peering is LAN-only: a device dials and admits only loopback,
private and link-local addresses.

The secret that seals a sync artifact is a 256-bit value from the channel's TLS exporter under
a peering label: fresh per session, never stored, never a user passphrase.

## Versioning

Peering negotiates like the server: each device advertises its supported
[`[Min, Max]` window](/design/versioning/#update-policy) and the pair speaks the highest
common version. Only disjoint windows end the connection, with a typed error telling the user
which device to update. Artifact formats are versioned by
[Backup and Recovery](/design/backup-recovery/#backup-artifact); upload bundles by the
[transfer protocol](/design/transfer/upload/#protocol-versioning).

## Sync

1. **Delta.** Each side offers its held content addresses and
   [sync cursor](/design/sync/#discovering-what-changed); the delta is the complement, honouring
   the receiver's [synchronization scope](/design/sync/#synchronization-scope). Offers are padded
   to a size bucket and capped per request.
2. **Pull.** The device that is behind pulls the artifact with ranged requests over the
   channel and verifies it against its content address, so a severed transfer resumes on the
   next channel without re-fetching. A peer may send an advisory
   [hint](/design/notifications/#vocabulary) that carries no authority.
3. **Ingest** through the backup restore path, **all-or-nothing per asset**: an asset applies
   only with its manifest, metadata and every blob its scope requires. A manifest behind the
   local chain head is quarantined as "peer sent stale state". **A purged asset is never
   resurrected**: the device keeps a tombstone for every asset it has purged, and ingest
   refuses any asset it has a tombstone for. A device that cannot read its tombstones refuses
   the ingest before it opens a session, never running it against an empty set. A deleted asset in
   [trash](/design/organization/#recycling) is not a tombstone, since a `trash-restore`
   still recovers it; its delete manifest is the local chain head, so a peer's older copy is
   quarantined as stale. A device learns of a purge from the
   [sync feed](/design/sync/#discovering-what-changed), which carries no purge entry: a
   delete entry carries its signed delete record, and an asset whose `retention_until` has
   passed can no longer be [restored](/design/organization/#retention-window), so it is
   purged. The device judges the deadline only once a sync has drained the feed to its head,
   so a restore on a later page is applied first and takes the asset out of the reckoning. It
   judges by its own clock, floored by the latest server `changed_at` the drain saw. A delete
   served with no record (after the purge, or from an older server) is no evidence of a purge,
   and records nothing.
4. **Reconcile.** Assets a device learned by peering are not uploaded again if the server
   holds them; [dedup and merge](/design/transfer/upload/#deduplication-and-merge) resolve
   them. An asset no device has uploaded is uploaded by whichever device holds it first, under
   the rules for delegation below.

## Delegation

A device with originals but a poor or metered link (a phone) hands them to a device with a
better one (a laptop), which uploads them to the server later. The delegate is an **opaque
courier**: it never holds album keys or plaintext for the delegated assets, and never signs on
their behalf.

```text
origin: seal + sign bundle ──hand-off──▶ delegate: persist, upload ──▶ server: receipt
   ▲                                                                       │
   └──────────── origin verifies the receipt against its own hash ◀───────┘
```

1. **Seal.** The origin builds the full upload bundle (encrypted original, derivatives, metadata
   blob, signed manifest) exactly as it would upload it, and records its own content hashes.
2. **Hand-off.** The delegate pulls the bundle over the channel, verifies every hash against the
   manifest, fsyncs it, and only then acknowledges over the same channel. The acknowledgement is
   not custody: the origin keeps the original pinned. Over the channel a peer serves only bundles
   it sealed itself, and the origin takes an acknowledgement only from the device it sealed the
   bundle for. The bundle is self-verifying, so the same hand-off may also travel by any other
   courier (the CLI writes it to a file); its integrity never rests on the channel.
3. **Upload.** The delegate uploads the bundle through the normal
   [transfer protocol](/design/transfer/upload/) under its own session. The server accepts a
   manifest signed by one device of the account and uploaded by another; quota is charged to
   the account as usual. The delegate never re-signs, re-encrypts or re-derives anything.
4. **Release.** The origin releases an original only when the server returns a
   [custody receipt](/design/transfer/durability/#custody-receipts) for the content hash **the
   origin computed itself**, and a read-back verify agrees. A receipt fetched through the
   delegate is not accepted.
5. **Cleanup.** The delegate deletes its copy after it holds the receipt. A delegate copy is
   never visible as a library asset on the delegate.

Edge cases:

| Case | Rule |
| --- | --- |
| Delegate dies, loses the bundle or withholds it | The origin sets a **monotonic** deadline at hand-off. On expiry, or when the delegate is revoked, it re-queues the upload itself or delegates again. |
| Both devices upload | Dedup resolves it; the receipt names the content hash, so either upload's receipt releases the original. |
| Origin deletes the asset after hand-off | The origin holds the signed delete locally until the create's receipt exists, then sends it; the server never sees a delete for an asset it has not indexed. |
| Origin edits metadata after hand-off | The edit is a new manifest the origin uploads itself; the delegated create stays valid. |
| Partial hand-off | Per asset, all or nothing: a delegate never uploads an incomplete bundle. |
| Delegate tampers with bytes | Hashes and the manifest signature fail at the server and at the origin's verify. |
| Delegate on an older protocol | It must speak the bundle's `protocol_version`; otherwise it refuses the hand-off and the origin keeps the upload. |
| Held hand-off permanently refused | The delegate never deletes a copy the server has not taken custody of. When a held hand-off can never succeed it is **quarantined**: the copy is kept byte for byte, no later run retries it, the delegate reports it once with a stable reason, and only the user deletes it. A revoked origin is often a lost device, so the delegate's copy may be the only one left. Never succeeds: the origin is revoked or gone from the directory after acceptance; the held bytes no longer verify; or the server refuses with `error.upload.envelope_rejected`, `envelope_mismatch` or `stale_revival` (a conflicting provenance), or at creation with `timestamp_out_of_range`, `album_access_denied` or `device_not_authorized`, which are the same conditions met one step earlier. `envelope_rejected` covers a manifest under a stale album epoch, a signer that is revoked, of another account or not verifying, a role conflict, a timestamp out of range, the album closed or the uploader's write access withdrawn since creation, and a signing device no longer authorized. The delegate cannot change a request the origin signed, so retrying cannot help. Any other failure (network, outage, quota, rate limit, this device's own directory state, a newer format) keeps the copy for the next run. Separately, the origin keeps its own original pinned and re-queues the upload at its deadline. |
| Delegate compromised | It learns what the server learns (ciphertext, sizes, timing), plus the origin's hand-off schedule. It cannot read or forge assets. |

The delegate follows the same battery, metering and scheduling policy as its own uploads
([Sync — scheduling](/design/sync/)). The origin shows each delegated asset as "handed to
{device}, awaiting server" until the receipt arrives.

## Robustness

Ranged transfers resume; a vanished peer equals no peer; reordering a transfer cannot change
state ([no order trust](/design/federation/#reconstructing-state-without-trusting-peers)).
Deadlines use monotonic time, so clock skew between devices changes nothing.

## Validation

- **Advertisement (unit):** no handle, device name or model; rotates per boot and per 24 h.
- **Handshake (unit):** same-IK succeeds; revoked device, foreign IK and a rolled-back
  directory fail; handshake rate limit holds; a table full of live sources, even one prefix's
  worth, still admits a new peer.
- **Version window (unit):** overlapping windows pick the highest common version; disjoint
  windows refuse with the typed error.
- **Delta (unit):** complement of two address sets; offers are padded and capped.
- **Ingest (smoke):** an artifact restores fully; a partial asset is not applied; a tombstoned
  asset is not resurrected; a stale chain head quarantines.
- **Revocation mid-session (smoke):** a peer revoked during a transfer is cut off at its next
  request and its later deliveries are quarantined.
- **Resume (smoke):** a severed transfer resumes without re-fetching.
- **Delegation (E2E):** origin hands off, delegate uploads, origin releases only on its own
  receipt check; plus each edge case in the table above, including delegate death, double
  upload, delete-after-hand-off and tampering.
- **LAN sync then server reconcile (E2E):** one case in
  [Module Map](/design/module-map/#e2e-test-surface).
