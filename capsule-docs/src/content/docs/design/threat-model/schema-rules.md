---
title: Schema Rules and Open Questions
description: Schema evolution, forbidden client behaviors, deprecation policy, and unresolved design questions
status: draft
---

Fields live in owner docs; this doc fixes how they may evolve.

## Schema Evolution and Field Grammar

[Postel's Law](/design/principles/#postels-law-asymmetric), asymmetric:

- **Requests:** unknown fields *inside* a known schema are preserved verbatim. Unknown **top-level** fields are rejected. An undeclared `sidecar_schema` or `crypto_suite_id` is rejected outright.
- **Responses:** unknown fields are preserved verbatim, so a newer server never breaks an older client.
- **Bounded sizes:** every field has a schema-declared maximum; oversize is rejected.

### Closed Enums

Every enum in a signed or validated structure is **closed per `protocol_version`**: an unknown value is a structural error, never a future value to ignore. Adding a value requires a new `protocol_version` ([album pinning](/design/versioning/#album-protocol-version-pinning)). Enforced by both server (invariant 16) and client decode ([Validation](/design/threat-model/validation/)). Value sets live in their owner docs ([actions](/design/authorization/#the-closed-action-set), [metadata enums](/design/metadata/#closed-enum-value-sets), [derivatives](/design/cryptography/provenance/#derivative-provenance), [stacks](/design/organization/#stack-membership-schema)).

### Timestamp Grammar

`timestamp`/`ts` fields are RFC 3339, **self-asserted and audit-only** — never used for authorization or ordering ([Write Authorization](/design/cryptography/keys/#write-authorization)). The server records its own `received_at` for time-based policy. A write-only drift bound (default ±30 days) surfaces broken clocks; not a security control.

## Forbidden Client Behaviors

Index of negated owner-doc rules; receivers reject the consequence structurally regardless.

| A correct client never…                                          | Owner                                                                                     |
| ---------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Re-signs under a lower `crypto_suite_id`                         | [Primitives](/design/cryptography/primitives/#versioning-identifiers)                     |
| Signs for an epoch whose write-tier key it lacks                 | [Keys](/design/cryptography/keys/#write-authorization)                                    |
| Removes an OR-set `add_id` it never observed                     | [Metadata](/design/metadata/#add-id-binding)                                              |
| Strips `_unknown` or `superseded_captions` on write-back         | [Metadata](/design/metadata/#collaborative-metadata)                                      |
| Overwrites or truncates a provenance chain                       | [Provenance](/design/cryptography/provenance/#chained-append-only-structure)              |
| Sends `revoke_all_sessions` without master-key proof             | [Authentication](/design/authentication/#explicit-revocation)                             |
| Decodes non-home-peer bytes outside the sandbox                  | [Clients](/design/clients/#sandboxed-decoder)                                             |
| Promotes an AI tag to a user tag except by a signed op           | [Metadata](/design/metadata/#tag-provenance-and-namespacing)                              |
| Retries `429`/`409`/`426` with the same payload                  | [Validation](/design/threat-model/validation/#protocol-and-capability-negotiation)        |

## Min-Supported-Client Deprecation Policy

1. **Announce** at `/.well-known/capsule/deprecation` at least the announcement window (default 90 days) before cutoff, naming the date and the minimum surviving `protocol_version`.
2. **Warn:** until cutoff, responses carry `X-Capsule-Min-Client-Build` and a `Warning:` header.
3. **Cut off:** the version leaves `[Min, Max]`; its writes get `426`, reads still succeed.
4. **No stranded data:** every [recovery path](/design/cryptography/failure-modes/) still works; only the old binary is stranded.

Never retroactive: albums pinned to a dropped version stay readable forever, just not writable.

## Open Questions

- **Cross-server album replication** (deferred to v2). v1 pins each album to one home server; the interim answer is the [aggregated album](/design/federation/#federated-shared-albums-aggregated-albums).
