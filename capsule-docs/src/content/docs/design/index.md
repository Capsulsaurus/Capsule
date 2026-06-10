---
title: Design Overview
description: How Capsule's design docs are organized and where to start
---

Capsule is an end-to-end-encrypted personal photo and media store with optional federation. These design docs are its normative specification: every primitive, schema, and protocol is declared in exactly one **owner doc** and referenced by anchor everywhere else (the [Single Source of Truth rule](/design/principles/#single-source-of-truth)).

## The shape of the system

The design stacks in layers, each building on the one below — the sidebar groups follow this order:

- **Foundations** — the [core principles](/design/principles/) every component obeys, and the [module map](/design/module-map/) from code module to owning doc.
- **Cryptography** — the [primitives](/design/cryptography/primitives/) inventory, the [key hierarchy](/design/cryptography/keys/), [MLS](/design/cryptography/mls/) group membership, asset/metadata [encryption](/design/cryptography/encryption/), and signed [provenance](/design/cryptography/provenance/). The server holds only opaque ciphertext — never a key.
- **Identity & access** — [authentication](/design/authentication/), [authorization](/design/authorization/), and [device enrollment](/design/device-enrollment/).
- **Storage** — the [server](/design/filesystem/server/) and [client](/design/filesystem/client/) filesystems, the [metadata](/design/metadata/) sidecar schema, and [thumbnails](/design/thumbnails/).
- **Import & sync** — the [import pipeline](/design/import/pipeline/), [upload protocol](/design/import/upload-protocol/), [download & sync](/design/import/download-sync/), [backup](/design/backup-recovery/), and [versioning](/design/versioning/).
- **Sharing & federation** — server-to-server [federation](/design/federation/), device-to-device [peering](/design/peering/), [share links](/design/share-links/), and [moderation](/design/moderation/).
- **Organization & clients** — [albums and stacks](/design/organization/), native [client duties](/design/clients/), and on-device [AI/ML](/design/ai/).
- **Threat model** — the cross-cutting [damage-scenario map](/design/threat-model/scenarios/), [validation invariants](/design/threat-model/validation/), and [schema rules](/design/threat-model/schema-rules/) that bound what a faulty or hostile client can do.

## Where to start

- **New to the project?** Read [Core Principles](/design/principles/), then the [Cryptography overview](/design/cryptography/).
- **Implementing a feature?** Find your code module in the [Module Map](/design/module-map/) — it names the owning design doc and the validation tier.
- **Reviewing security?** Start at the [Threat Model](/design/threat-model/) and follow each damage scenario to the owner doc that defeats it.
