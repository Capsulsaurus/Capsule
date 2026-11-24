---
title: Peering
description: Direct device-to-device sync and upload delegation between one user's own devices
audit: none
---

Peering connects **one user's own devices** directly; server-to-server sharing is [](/design/federation/). It has two uses:

| Use            | What moves                                          | Who ends up holding it                        |
| -------------- | --------------------------------------------------- | --------------------------------------------- |
| **Sync**       | A delta of what one device has and the other lacks  | Both devices                                  |
| **Delegation** | Sealed upload bundles a device has not yet uploaded | The delegate, until the server issues custody |

Peering is an **accelerator, never a replacement** for the server. It moves bytes at LAN speed and lets devices converge while the server is unreachable, but a copy on another device is **never custody**: only a server custody receipt releases a local original (invariant 7 of [](/design/versioning/#system-invariants)). Implementation: `capsule-sdk::peering`.

## Payloads

Peering adds no payload format and no sync engine. It owns only discovery, the channel and the hand-off rules below.

- **Sync** moves a delta-scoped backup artifact ([](/design/backup-recovery/#backup-artifact)), ingested through the backup restore path.
- **Delegation** moves upload bundles exactly as the transfer protocol defines them ([](/design/transfer/upload/#what-gets-uploaded)): ciphertext blobs plus the signed manifest, sealed and signed by the originating device before they leave it.

## Trust Model

Both ends hold device keys cross-signed into the same device directory ([](/design/cryptography/keys/#device-directory)). Identity-trusted is **not** content-trusted: everything received is re-verified (content hash, STREAM tags per [](/design/cryptography/encryption/#stream-construction), `verify_asset` per [](/design/cryptography/keys/#write-authorization)), so a peer can at worst withhold or delay what it carries, never alter it.

- **Directory anti-rollback.** Each device persists the highest `directory_version` it has verified and refuses a peer presenting a lower one, so a stale directory cannot readmit a revoked device.
- **Revocation is checked on every request**, not only at the handshake. A device removed from the directory mid-session is cut off at its next request, and everything it delivered after its revocation took effect is quarantined.
- **Each device is its own containment boundary.** A manifest behind the local chain head is quarantined, and any `sidecar_schema`, `crypto_suite_id` or `protocol_version` above the receiver's maximum is rejected at decode.

## Discovery

Devices advertise over **mDNS** and accept **TCP**, **LAN-only**, with no relay. The advertisement MUST NOT identify the user or device: it is an **opaque instance name**, rotated at every boot and at least every 24 h, never a handle, device name or model. Same-user is established inside the channel, and no answer means silent fallback to the server. Handshake attempts are rate-limited per source address, so the LAN cannot probe a device for membership.

The limiter tracks a bounded number of source addresses. A new source that finds the table full of live ones **evicts the least recently active source** and is never refused. The limiter keys on the **full address**, not a prefix: on a LAN every IPv6 peer shares the link's /64 (and every link-local peer shares `fe80::/64`), so keying by prefix would let one host exhaust every peer's budget on the link. The trade-off is that a host holding more addresses than the table tracks can flush the oldest entries and reset their budgets. That weakens the limit only for a host that already gets a fresh budget with every new address, and never locks a peer out.

The live mDNS responder ships behind the `Discovery` seam, tested in process.

## Establishing the Channel

The channel is a framed request/response exchange over **mutually authenticated TLS 1.3** with no CA. Both devices must already be enrolled ([](/design/device-enrollment/)).

- **Identity.** TLS authenticates the device key's **classical half**. The **hybrid** proof that the key chains to the shared User IK (per [](/design/cryptography/primitives/#signature-scheme)) is bound to the TLS exporter and verified before any payload byte.
- **No reflection.** The proof also covers the sender's role, dialer or listener, so the two hellos of one session sign different messages and a hello reflected back at its sender never verifies. A hello naming the receiving device's own id is refused outright: a device never peers with itself.
- **No plaintext path.** Every payload byte travels inside the verified TLS session.
- **Bounded.** Each frame's length is checked against a fixed ceiling before it is read, every read and write is bounded in time, and a ranged pull must sustain a minimum transfer rate.
- **Authorized per request.** Every decision is made against the identity the handshake verified, never a device id a frame claims. The serving device lists what it offers, then authorizes **each request** against the directory it pins at that moment, so a peer revoked mid-session is refused at its next request.
- **LAN-only.** A device dials and admits only loopback, RFC 1918 private and link-local IPv4 addresses, and IPv6 unique-local and link-local addresses (`is_lan` in `capsule-sdk::peering`). The carrier-grade NAT range `100.64.0.0/10` is shared address space, not a LAN, and is refused, as is every public address.
- **The LAN is a reachability filter, not a trust boundary.** A hotel, campus or office network puts strangers on the same private range, and they can see the advertisement and attempt a handshake. What they learn is that some Capsule device is present (the service type), never whose: the instance name is opaque and rotating, and the hello's identity is sent only inside TLS. What they can do is spend the per-source handshake budget, and the mTLS hybrid check against the device directory still refuses them. A device SHOULD stop advertising on a network the platform reports as public or captive, and the user can turn advertising off per network.

The secret that seals a sync artifact is a 256-bit value from the channel's TLS exporter under a peering label: fresh per session, never stored, never a user passphrase.

## Wire Format

The peer channel is the one Capsule surface that is not an HTTP API, so it has no OpenAPI contract and no generated client. This section is its contract, precise enough that a second implementation interoperates and golden fixtures pin it. Every constant below is protocol surface: changing one is a new peering protocol version ([](#versioning)). The channel is `capsule-sdk::peering::channel`; the offer page is `capsule-sdk::peering::delta`.

**Transport.** One TCP connection carries one TLS 1.3 session with mandatory client authentication. Each side presents a fresh self-signed leaf certificate; certificate chains are not validated, TLS handshake signatures are. The client sends the SNI `capsule-peer.local`, which nothing checks. No ALPN is offered. The TLS policy is [](/design/networking/#transport-security).

**Exporters.** Both are RFC 5705 exporters of 32 bytes with a 32-byte all-zero context:

| Label                                  | Use                                               |
| -------------------------------------- | ------------------------------------------------- |
| `EXPORTER-capsule-peering-hybrid-v1`   | The channel binding each hello's proof signs      |
| `EXPORTER-capsule-peering-artifact-v1` | The per-session secret that seals a sync artifact |

**Hello.** Each side sends exactly one hello, framed as a big-endian `u32` length followed by that many bytes of JSON, at most 64 KiB. The listener reads first and then writes; the dialer writes first and then reads. The whole handshake (TLS, both hellos and the hybrid check) is bounded at 15 s.

| Field                  | Type                           | Meaning                                                                                                                                                      |
| ---------------------- | ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `device_id`            | UUID string                    | The sender's directory device id                                                                                                                             |
| `peering_protocol`     | `YYYY-MM-DD`                   | Top of the sender's window                                                                                                                                   |
| `peering_protocol_min` | `YYYY-MM-DD`, optional         | Bottom of the window; absent means equal to the top                                                                                                          |
| `directory_version`    | integer, default `0`           | The directory version the sender pins                                                                                                                        |
| `proof`                | `{ "ed": bytes, "ml": bytes }` | The hybrid signature ([](/design/cryptography/primitives/#signature-scheme)): the classical half (64-byte Ed25519, or DER ECDSA-P256) and the ML-DSA-65 half |

The proof signs `"capsule-peering-hello-v1/" ‖ role ‖ 0x00 ‖ exporter`, where `role` is the ASCII `dialer` or `listener` of the **sender**. The receiver checks, in order: the version window, that the directory chains to the User IK and does not roll back, that the peer's directory is not stale, that the hello does not name the receiver, that the device is enrolled and unrevoked, and then the signature.

**Frames after the hello.** Every frame is `tag: u8 ‖ length: u32 (big-endian) ‖ payload`. The receiver checks the tag and the length against the tag's ceiling **before** it allocates or reads the payload; a wrong tag or an oversized length ends the channel.

| Tag         | Payload                                                | Ceiling |
| ----------- | ------------------------------------------------------ | ------- |
| `1` control | One JSON message object, below                         | 64 KiB  |
| `2` data    | Raw bytes of the ranged answer or offer page in flight | 1 MiB   |

**Control messages.** Each is a JSON object whose `type` member names it. The exchange is strictly request–response: the pulling side sends one request and reads its whole answer before it sends the next.

| `type`        | Direction              | Members                                                                                     | Answer                                       |
| ------------- | ---------------------- | ------------------------------------------------------------------------------------------- | -------------------------------------------- |
| `offer`       | puller → server, first | `pages`: count of offer-page data frames that follow, at most 256; `0` offers nothing       | `listing`                                    |
| `listing`     | server → puller        | `artifacts`: at most 256 of `{ address, len }` (SHA-256 lowercase hex, byte length ≤ 4 GiB) | —                                            |
| `range`       | puller → server        | `address`, `start`, `max_len` (optional)                                                    | `range_reply` then data frames, or `refused` |
| `range_reply` | server → puller        | `len`: bytes that follow, at most 16 MiB                                                    | —                                            |
| `ack`         | delegate → origin      | `ack`: `{ asset_id, hand_off, delegate_device }`                                            | `acked` or `refused`                         |
| `acked`       | origin → delegate      | none                                                                                        | —                                            |
| `refused`     | server → puller        | `kind` (stable name), `message` (English detail)                                            | —                                            |
| `done`        | puller → server        | none                                                                                        | channel closes                               |

**Offer page.** A data frame holding a 12-byte header (`cursor: u64`, `count: u32`, both big-endian) and then 32-byte address slots, the first `count` of them real and the rest zero padding up to a power-of-two bucket of at least 64 slots. A page holds at most 4,096 addresses.

**Byte strings in JSON** are arrays of integers 0–255, the JSON form the shared signature type serializes to. This is the representation the golden fixtures pin, not an incidental serializer choice: renaming a Rust field or switching a serializer is a wire change and fails the fixtures.

**Refusal kinds** are stable snake_case names drawn from the peering and delegation error sets, plus `not_offered` (a `range` for an address the listing did not name) and `range_not_satisfiable`. A receiver treats an unknown kind as a refusal it cannot act on.

**Timing.** Every frame read and write after the handshake is bounded at 30 s. A ranged answer of `len` bytes must arrive whole within that bound plus `len` at 64 KiB/s, so a trickling peer cannot hold the puller. While an offered hand-off is unacknowledged, the serving side waits the frame bound plus the largest hand-off's length at 1 MiB/s for the delegate's next request, so verifying and persisting a large hand-off on slow storage does not time out.

**Golden fixtures.** For every peering protocol version the build speaks there is a byte-exact fixture of each hello, each control message, an offer page and a data frame, generated from fixed keys and a fixed exporter, and both ends' decoders MUST accept exactly those bytes. A change that alters any fixture without a new version fails the build.

## Versioning

Peering negotiates like the server. Each device advertises its supported `[Min, Max]` window ([](/design/versioning/#update-policy)) and the pair speaks the highest common version. Only disjoint windows end the connection, with a typed error telling the user which device to update. Peering versions are date strings in the server protocol's grammar but are **their own version space**: they compare only with peering versions. Artifact formats are versioned by [](/design/backup-recovery/#backup-artifact), and upload bundles by the transfer protocol ([](/design/transfer/upload/#protocol-versioning)).

## Sync

1. **Delta.** The receiver opens the session with its offer: the head record hash of every asset it holds, and its sync cursor ([](/design/sync/#discovering-what-changed)). The sender cuts its artifact to the complement, honouring the receiver's synchronization scope ([](/design/sync/#synchronization-scope)), so an asset the receiver holds at the same head is never sent. Offers are padded to a size bucket, capped per page and capped in pages. The offer is an additive protocol version: a pair on an older version exchanges none, and the sender sends everything it would have offered, which ingest makes a no-op for each asset the receiver already holds.
2. **Pull.** The device that is behind pulls the artifact with ranged requests over the channel and verifies it against its content address, so a severed transfer resumes on the next channel without re-fetching. A peer may send an advisory hint ([](/design/notifications/#vocabulary)) that carries no authority.
3. **Ingest** through the backup restore path, **all-or-nothing per asset**: an asset applies only with its manifest, metadata and every blob its scope requires. A manifest behind the local chain head is quarantined as "peer sent stale state".
4. **Reconcile.** Assets a device learned by peering are not uploaded again if the server holds them; dedup and merge ([](/design/transfer/upload/#deduplication-and-merge)) resolve them. An asset no device has uploaded is uploaded by whichever device holds it first, under the delegation rules below, unless it is in the trash: a device that learns of a delete over peering uploads no create for it, and holds the delete until another upload of the create is indexed.

**A purged asset is never resurrected.** The device keeps a tombstone for every asset it has purged, its kept provenance chain ([](/design/filesystem/client/#desktop-library-layout)), and ingest refuses any asset it has a tombstone for.

- A device that cannot read its tombstones refuses the ingest before it opens a session, never running it against an empty set.
- A deleted asset in trash ([](/design/organization/#recycling)) is not a tombstone, since a `trash-restore` still recovers it. Its delete manifest is the local chain head, so a peer's older copy is quarantined as stale.
- A device learns of a purge from the sync feed ([](/design/sync/#discovering-what-changed)), which carries no purge entry. A delete entry carries its signed delete record, and an asset whose `retention_until` has passed can no longer be restored ([](/design/organization/#retention-window)), so it is purged.
- The device judges the deadline only once a sync has drained the feed to its head, so a restore on a later page is applied first and takes the asset out of the reckoning.
- It judges by the server's clock as the drain saw it, the latest `changed_at`, never its own. A clock running ahead would tombstone the asset early and then refuse a restore the server accepted in time.
- An asset whose delete is still held (the server has not indexed it) has no window running on the server and is never purged on expiry ([](/design/organization/#deletion-state-machine)).
- A delete served with no record (after the purge, or from an older server) is no evidence of a purge and records nothing.

## Delegation

A device with originals but a poor or metered link (a phone) hands them to a device with a better one (a laptop), which uploads them later. The delegate is a **courier**: the delegation path never decrypts, re-signs, re-encrypts or re-derives anything, and the courier uploads the origin's sealed bytes byte for byte. That is a property of the path, not a confinement of the device: a courier is always a device of the same account, and such a device already holds the account's keys and can open every album the account can ([](/design/cryptography/keys/)). Delegation gives it nothing it did not have. A sealed bundle a user carries between devices as a file is this hand-off, never a plaintext export ([](/design/export/)). Implementation: `capsule-sdk::peering::delegation`.

```mermaid title="Delegated upload: registered with the server, released only on the origin's own receipt check"
flowchart TB
    origin["Origin<br/>seal, sign, register"]:::client -->|bundle file<br/>LAN, USB, AirDrop| delegate["Courier<br/>verify, persist, start, upload"]:::client
    origin -->|register hand-off| server["Server"]:::server
    delegate -->|start, upload| server
    server -->|custody receipt| check["Origin verifies receipt<br/>against its own hash"]:::accent
    server -->|custody receipt| drop["Courier drops its copy"]:::client
```

1. **Seal.** The origin builds the full upload bundle (encrypted original, derivatives, metadata blob, signed manifest) exactly as it would upload it, and records its own content hashes.
2. **Register.** The origin registers the hand-off with the server before it releases the bundle ([](#hand-off-registration)). The server's answer fixes the hand-off's start deadline on the server's clock.
3. **Hand-off.** The courier receives the bundle file ([](#the-bundle-file)) over the peer channel or any other carrier, verifies it whole, and persists it durably before it acknowledges. The acknowledgement is not custody, so the origin keeps the original pinned. Over the channel a peer serves only bundles it sealed itself, and the origin takes an acknowledgement only from the device it sealed the bundle for. Integrity never rests on the carrier.
4. **Start and upload.** The courier starts the registered hand-off, then uploads every member through the normal transfer protocol ([](/design/transfer/upload/)) under its own sessions, each create naming the hand-off. The server accepts a manifest signed by one device of the account and uploaded by another, and charges quota to the account as usual. Hand-off uploads run in the scheduler's `backup` class ([](/design/transfer/download/#transfer-scheduler)).
5. **Release.** The origin releases an original only through the verify-before-destroy gate ([](/design/transfer/durability/#verify-before-destroy)) over custody **it fetched itself** for the content hashes **it computed itself**, with a read-back of its own copy. A receipt relayed by the courier is never consulted.
6. **Drop.** The courier deletes its copy only once it has fetched, with its own session, a verified custody receipt and a `durable` verdict (with `deep = intact` for the original, under the deep-verify budget of [](/design/authorization/#rate-limits)) for **every** member. It does not care who uploaded them: custody of the content is what matters. A courier copy is never visible as a library asset on the courier.

### Hand-off Registration

The server cannot tell which device of an account sent a request ([](/design/transfer/upload/#quota-and-permissions)), so a hand-off's parties are proven by device signatures checked against the published device directory, the way a manifest's signer is.

- **Register.** The origin sends a registration signed by its device key: a version, a hand-off id (UUIDv7), the asset id, the **bundle address** (SHA-256 of the bundle file), every member's content hash, the origin and courier device ids, a **start window** and a **liveness window**, both durations. The server verifies the signature and that both devices are enrolled and unrevoked, and records the hand-off as `registered` with `start_by` = now + start window on its own clock. The origin hands the bundle off only after the server has answered.
- **Start.** Before its first create, the courier sends a start signed by its own device key, naming the hand-off id and the bundle address it computed over the file it holds. The server admits it only if the hand-off is `registered`, the courier is the one registered and is unrevoked, the address matches, and its clock is before `start_by`. Otherwise it refuses with `error.upload.handoff_closed`, and a late start forfeits the hand-off.
- **Progress.** Each create of the courier names the hand-off. While it is `started`, every range recorded and every finalization in those sessions is progress. A hand-off with no progress for its liveness window is **forfeited**, and so is one whose courier is revoked, checked at its next request. One whose every member is `Completed` is **completed**.
- **Closed is closed.** Once a hand-off is forfeited, withdrawn by the origin, or expired unstarted, every create naming it is refused `error.upload.handoff_closed`. The courier stops, keeps its copy, and drops it once custody of every member exists, by whoever's upload.
- **The origin re-queues.** The origin reads the hand-off's state with its own session. On `forfeited`, or `registered` past `start_by`, it withdraws the hand-off and uploads the asset itself or registers a new hand-off to another courier, unless the asset is in the trash: no asset is uploaded only so that its delete can follow.
- **Coordination, not access control.** Forfeiture tells an honest courier to stop and the origin that it may take over without racing it. It does not stop a device of the same account from uploading the same signed bytes without naming the hand-off, which any device of the account may always do. Such an upload changes nothing: the bytes are the origin's, signed by the origin, and deduplicate by address.
- **Clocks.** `start_by` and the liveness window are judged only on the server's clock and travel only as durations; no device compares its wall clock with another's.
- **Retention.** A hand-off record is expiring state with its own `expires_at`, 30 days after it reaches a terminal state ([](/design/filesystem/server/#required-services)).

Register, read, withdraw and start are named operations of the upload surface; their shapes are the API reference's once they exist.

### The Bundle File

A bundle is **one file** holding one asset's hand-off, so it can travel by any carrier: the peer channel's ranged pull, a USB stick, AirDrop, a shared folder.

- **Layout.** The magic `CAPSHOF1`, the container format, a header length, a JSON header (format, hand-off id, asset id, origin and courier device ids, protocol version, and each member's upload request with its size and content hash), then each member's bytes in header order. Every member begins with its own blob header ([](/design/cryptography/encryption/#blob-header)). The container is not signed; its integrity comes from the members' addresses, the origin-signed manifest inside it, and the bundle address the registration names.
- **Written atomically.** The writer streams it to a temporary file in the destination directory, fsyncs it, renames it into place and fsyncs the directory, so a reader never sees a partial bundle under its final name. A receiver verifies before it renames into its own inbox, so a bundle that fails verification never becomes a held copy.
- **Checked without a key.** A quick check reads only headers: the magic, the container header, member sizes that sum to the file's length, and each member's blob header at its offset (known magic, version and suite). It catches a truncated or wrong file at once. The full check, required before acknowledging, hashes every member against its declared address, verifies the manifest's signature under the origin's published key, checks each member's header against the manifest ([](/design/cryptography/encryption/#header-and-manifest-agreement)), and hashes the file against the bundle address. Nothing is decrypted.
- **Bounded memory.** Writing, checking and uploading a bundle stream it from disk; no step holds a whole member in memory.

### Threat Analysis

| Threat                                                      | What it can do                                           | What stops it                                                                                                                                                                                               | Residual                                                                                             |
| ----------------------------------------------------------- | -------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| **Malicious courier** (a compromised device of the account) | Withhold, delay or lose the bundle                       | The start deadline and liveness window forfeit it on the server's clock; the origin re-queues and never releases without custody it fetched itself                                                          | Delay up to the start window plus one liveness window                                                |
|                                                             | Tamper with bytes or the container header                | Member hashes, the manifest signature, header–manifest agreement and the registered bundle address fail at the courier's own check, at the server's start and finalize, and against the origin's own hashes | None                                                                                                 |
|                                                             | Read the assets                                          | Nothing in delegation: a device of the account already holds the account's keys                                                                                                                             | Containment is device revocation and epoch rotation ([](/design/cryptography/keys/)), not delegation |
|                                                             | Forge a registration or claim another courier's hand-off | Registration needs the origin's device signature, a start the registered courier's                                                                                                                          | None                                                                                                 |
| **Replayed bundle**                                         | Redelivered to the same courier                          | The inbox is idempotent by bundle address                                                                                                                                                                   | None                                                                                                 |
|                                                             | Delivered to a different device                          | The header and the registration name the courier; another device's start is refused                                                                                                                         | None                                                                                                 |
|                                                             | Started after forfeiture, withdrawal or expiry           | `handoff_closed`                                                                                                                                                                                            | None                                                                                                 |
|                                                             | Uploaded after the origin uploaded it itself             | Deduplication by address: `duplicate_blob`, no second charge, and the courier drops its copy on custody                                                                                                     | None                                                                                                 |
|                                                             | Uploaded after the asset was deleted                     | The origin holds the delete until the create is indexed; a create behind a later chain head is `stale_revival`, which the courier quarantines                                                               | None                                                                                                 |
| **Revoked courier**                                         | Start or continue a hand-off                             | The start checks the directory; a started hand-off is forfeited at the courier's next request, and the origin re-queues                                                                                     | Members it already finalized stand: they are the origin's signed bytes                               |
| **Revoked origin**                                          | Have its bundle uploaded                                 | Finalization verifies the signer against the directory and refuses a revoked one                                                                                                                            | The courier quarantines its copy, which may be the last one ([](#delegation))                        |

Edge cases:

- **Courier dies, loses the bundle or withholds it.** The server forfeits the hand-off at its start deadline or liveness window, and the origin re-queues as above. The origin also keeps a monotonic local deadline, but only to tell the user a hand-off is overdue; it acts on the server's state.
- **Both devices upload.** Dedup resolves it. The receipt names the content hash, so either upload's receipt releases the original.
- **Origin deletes the asset after hand-off.** The origin holds the signed delete locally until the create's receipt exists, then sends it, so the server never sees a delete for an asset it has not indexed. The delegate does not drop its copy on learning of the delete: it never deletes a copy the server has not taken custody of, and the held delete supersedes the create once it lands.
- **Delegate uploads after the origin purged.** The held delete outlives the purge in the origin's tombstone, headed by the shorter-retention re-issue the purge appended, and is sent from it once the create is indexed. The server then purges by that window.
- **Origin edits metadata after hand-off.** The edit is a new manifest the origin uploads itself; the delegated create stays valid.
- **Partial hand-off.** All or nothing per asset: a delegate never uploads an incomplete bundle.
- **Delegate tampers with bytes.** Hashes and the manifest signature fail at the server and at the origin's verify.
- **Delegate on an older protocol.** It must speak the bundle's `protocol_version`; otherwise it refuses the hand-off and the origin keeps the upload.
- **Delegate compromised.** Through delegation it learns what the server learns (ciphertext, sizes, timing) plus the origin's hand-off schedule, and it cannot forge assets, since it cannot sign as the origin. As a device of the account it can read whatever the account can, with or without delegation ([](#threat-analysis)).
- **Held hand-off permanently refused.** The delegate never deletes a copy the server has not taken custody of. A held hand-off that can never succeed is **quarantined**: the copy is kept byte for byte, no later run retries it, the delegate reports it once with a stable reason, and only the user deletes it. A revoked origin is often a lost device, so the delegate's copy may be the only one left. Separately, the origin keeps its own original pinned and re-queues the upload once the server reports the hand-off forfeited, unless the asset is in the trash.
  - _Closed:_ `error.upload.handoff_closed` is neither. The courier stops uploading and keeps the copy only until custody of every member exists, whoever uploads it ([](#hand-off-registration)).
  - _Never succeeds:_ the origin is revoked or gone from the directory after acceptance; the held bytes no longer verify; or the server refuses with `error.upload.envelope_rejected`, `envelope_mismatch` or `stale_revival` (a conflicting provenance), or at creation with `timestamp_out_of_range`, `album_access_denied` or `device_not_authorized`, the same conditions met one step earlier. The delegate cannot change a request the origin signed, so retrying cannot help.
  - `envelope_rejected` covers a manifest under a stale album epoch; a signer that is revoked, of another account or not verifying; a role conflict; a timestamp out of range; the album closed or the uploader's write access withdrawn since creation; and a signing device no longer authorized.
  - _Retried next run:_ any other failure (network, outage, quota, rate limit, this device's own directory state, a newer format) keeps the copy.

The delegate follows the same battery, metering and scheduling policy as its own uploads ([](/design/sync/#auto-syncing)). The origin shows each delegated asset as "handed to {device}, awaiting server" until the receipt arrives, and "hand-off overdue" once the server reports it forfeited.

**Known gaps.** The server has no hand-off registration yet: the origin judges the hand-off only by its own monotonic deadline (24 h by default), so a late or stalled courier is never refused and the origin cannot tell a courier that is uploading from one that is not. A hand-off is held whole in memory while it is encoded, checked and sent, and its container does not yet carry a hand-off id.

## Robustness

Ranged transfers resume, a vanished peer equals no peer, and reordering a transfer cannot change state ([](/design/federation/#reconstructing-state-without-trusting-peers)). Local deadlines use monotonic time and hand-off deadlines the server's clock, so clock skew between devices changes nothing.

## Validation

- **Advertisement (unit):** no handle, device name or model; rotates per boot and per 24 h.
- **Handshake (unit):** same-IK succeeds; a revoked device, foreign IK and rolled-back directory fail; the handshake rate limit holds; a table full of live sources, even one prefix's worth, still admits a new peer.
- **Version window (unit):** overlapping windows pick the highest common version; disjoint windows refuse with the typed error.
- **Delta (unit):** complement of two address sets; offers are padded and capped.
- **Offer exchange (smoke):** a receiver's offer crosses the channel whole before the listing; a receiver holding every head is sent an artifact with no assets; a receiver that sends no offer, or an older peer, is sent everything.
- **Ingest (smoke):** an artifact restores fully; a partial asset is not applied; a tombstoned asset is not resurrected; a stale chain head quarantines.
- **Revocation mid-session (smoke):** a peer revoked during a transfer is cut off at its next request and its later deliveries are quarantined.
- **Resume (smoke):** a severed transfer resumes without re-fetching.
- **Delegation (E2E):** the origin registers and hands off, the courier starts and uploads, and the origin releases only on its own receipt check; plus each edge case above, including courier death, double upload, delete-after-hand-off and tampering.
- **Hand-off registration (unit):** a late start, a start by another device, a start with a different bundle address and a start by a revoked courier are refused; a started hand-off with no progress for its liveness window is forfeited; a create naming a closed hand-off is `handoff_closed`.
- **Bundle file (unit):** a truncated bundle fails the quick check; a flipped byte in any member, the container header or a blob header fails the full check; an interrupted write leaves nothing under the final name.
- **Wire fixtures (unit):** every golden fixture of every supported peering version decodes, and re-encodes byte for byte.
- **LAN filter (unit):** RFC 1918, link-local, ULA and loopback addresses are admitted; `100.64.0.0/10` and public addresses are refused.
- **LAN sync then server reconcile (E2E):** one case in the [E2E case index](https://github.com/Capsulsaurus/Capsule/blob/master/capsule-e2e/src/lib.rs).
