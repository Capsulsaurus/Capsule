---
title: Failure Modes and Recovery
description: What can go wrong with Capsule's cryptographic state, and the independent paths that restore it
status: draft
---

Owns the **failures Capsule must survive** and the **independent recovery paths** that must stay workable.

## Failure Mode Catalog

| Failure | Detected / contained by | Recovery |
| --- | --- | --- |
| Master key loss | — | Path 1 or 2 |
| Device key loss | Device keys are disposable | Re-bootstrap (1/2); never recovered |
| AMK loss | — | Path 3 or 4 |
| Write-tier key loss | — | Re-minted next epoch |
| Master key compromise | — | [Rotate and re-key](/design/cryptography/keys/#master-key-compromise) |
| Device compromise | — | Revocation certificate + MLS `Remove`; survivors rotate |
| AMK / write-tier compromise | — | Epoch bump; old epoch cannot read or sign future ones |
| Server compromise | Server never trusted | Authz from MLS history; E2E data |
| One half of a hybrid primitive broken | Hybrid construction | Other half holds |
| Ciphertext corruption / truncation / reorder | STREAM tags + manifest `ciphertext_hash` | Re-fetch a content-addressed copy (6) |
| Reader-signed / removed-writer / wrong-epoch / forged / replayed manifest | [`verify_asset`](/design/cryptography/keys/#write-authorization) | Quarantine + [audit trail](/design/cryptography/provenance/#provenance-of-library-modifications) |
| AMK distribution lag | `verify_asset` *pending* | Retry; quarantine after timeout |
| MLS ratchet corruption | — | Paths 1, 3, 4; [MLS Resilience](/design/mls-resilience/) |
| Backup missing an `amk_version` | AMK-completeness check | Re-export before reliance |
| Nonce reuse | Structural ([Re-keying](/design/cryptography/encryption/#re-keying-on-rewrite)) | — |
| CBOR non-determinism | Canonical encoding | Byte-identical re-encode |
| Bug corrupting the library DB | DB is a rebuildable cache | Rebuild from sidecars (5) |
| Erroneous delete | Soft-delete default | Restore from trash (7) |
| Stale revival (old valid manifest) | `prior_provenance_hash` chain, client and [server](/design/threat-model/validation/) | Quarantine; chain advance refused |
| Suite downgrade | Signature covers `crypto_suite_id`, `protocol_version` | Quarantine |
| Derivative poisoning | [`DerivativeManifest`](/design/cryptography/provenance/#derivative-provenance) chain | Unsigned overwrite rejected |
| Old client strips unknown sidecar fields | Signature covers all bytes; refuse writes above max `sidecar_schema` | Quarantine |

## Redundant Recovery Paths

Independent paths:

| # | Path | Survives |
| --- | --- | --- |
| 1 | [Master-key escrow](/design/backup-recovery/#master-key-escrow): passphrase/seed → master key → AMK escrow | Total device loss |
| 2 | Cross-device recovery ([Device Enrollment](/design/device-enrollment/)) | Partial device loss; lost master-key backup if one device survives |
| 3 | [OGK](/design/cryptography/keys/#owner-group-keys-ogks): any owner recovers every album's AMKs | Lost membership, AMK distribution gaps |
| 4 | Offline [backup artifact](/design/backup-recovery/#backup-artifact) | Server loss, account compromise, escrow corruption |
| 5 | Filesystem rebuild: CBOR sidecars are canonical, DB is a cache (idempotent `rebuild_index()`) | DB corruption, index bugs |
| 6 | Content-addressed copies on devices or [federated](/design/federation/) peers (ciphertext, not keys) | Single-server data loss |
| 7 | Trash: signed soft `delete` with a retention window before purge | Erroneous deletes |

**Account types.** Registered accounts have all seven. [Delegated/sponsored](/design/authentication/#account-types) accounts recover only through the sponsor's master key ([consequence](/design/cryptography/keys/#delegatedsponsored-accounts)). Non-registered [share-link](/design/share-links/) accounts have no collection to recover.

## Bug-Resistance Invariants

- Backup and restore never touch MLS ratchet state; the master key is the single backed-up root.
- Device keys are hardware-bound, non-exportable, never backed up; the master identity cross-signs every device.
- Every construction is versioned (KDF `info`, in-blob Argon2id params, [`crypto_suite_id`](/design/cryptography/primitives/#versioning-identifiers), [`sidecar_schema`](/design/metadata/#sidecar-schema-v1)), so v2 never strands v1.
- `verify_asset` quarantines, never drops or silently accepts.
- Provenance is append-only and hybrid-signed per record ([Provenance](/design/cryptography/provenance/#provenance-of-library-modifications)).
- [Backup verification](/design/backup-recovery/#backup-verification) runs before the backup is relied on.

## Transport Security

- TLS 1.3 required wherever both ends support it; TLS 1.2 is the absolute floor (legacy ingress only); nothing lower. Clients try 1.3 first.
- Deployments SHOULD enable hybrid X25519+ML-KEM key exchange on 1.3.
- Ingress (not the app server) terminates TLS: operator-configured, documented not enforced.
- Capsule's own TLS (SDK, [LAN peering](/design/peering/) mTLS, egress) is rustls ([Dependencies](/design/dependencies/#rust)), same policy.

## Validation

- Every catalog row tests detection and recovery; CI fails a row without one.
- One scenario per path: reach the lost state, recover, assert every asset returns.
- Exported backup artifacts contain no ratchet state; device private keys are non-exportable per platform ([Keys](/design/cryptography/keys/#validation)); v1↔v2 sidecar round-trip.
- System-level coverage: [Module Map — E2E Test Surface](/design/module-map/#e2e-test-surface).
