---
title: Design Overview
description: How Capsule's design docs are organized and where to start
status: stable
---

Capsule is an end-to-end-encrypted personal photo and media store with optional federation. These design docs are its normative specification. Every primitive, schema and protocol is declared in exactly one **owner doc** and referenced by anchor everywhere else ([](/design/principles/#single-source-of-truth)).

## The shape of the system

The design stacks in layers, each building on the one below. The sidebar groups follow this order.

- **Foundations:** the [core principles](/design/principles/) every component obeys, the [](/design/api-surfaces/) map from server surface to transport, and how those surfaces become [published reference](/design/developer-docs/).
- **Cryptography:** the [primitives](/design/cryptography/primitives/) inventory, the [key hierarchy](/design/cryptography/keys/), [MLS](/design/cryptography/mls/) group membership with its [resilience contracts](/design/mls-resilience/), asset and metadata [encryption](/design/cryptography/encryption/), and signed [provenance](/design/cryptography/provenance/). The server holds only opaque ciphertext, never a key.
- **Identity and access:** [](/design/authentication/), [](/design/authorization/) and [](/design/device-enrollment/).
- **Storage:** the [server](/design/filesystem/server/) and [client](/design/filesystem/client/) filesystems, the [metadata](/design/metadata/) sidecar schema, [thumbnails](/design/thumbnails/) and [](/design/quota/).
- **Import and sync:** the [](/design/import/pipeline/), [upload protocol](/design/transfer/upload/), [download and sync](/design/transfer/download/), [storage verification](/design/transfer/durability/), [backup](/design/backup-recovery/) and [](/design/versioning/).
- **Sharing and federation:** server-to-server [federation](/design/federation/), device-to-device [peering](/design/peering/), [](/design/share-links/), [](/design/web-upload/) for guest drops, and [](/design/moderation/).
- **Organization and clients:** [albums and stacks](/design/organization/), native [client duties](/design/clients/), how a key-free server reaches a user at all ([](/design/notifications/)), [](/design/i18n/), and on-device [AI/ML](/design/ai/).
- **Threat model:** the cross-cutting [damage-scenario map](/design/threat-model/scenarios/), [](/design/threat-model/validation/) and [schema rules](/design/threat-model/schema-rules/) that bound what a faulty or hostile client can do.

## Where to start

- **New to the project?** Read [](/design/principles/), then the [](/design/cryptography/) overview.
- **Implementing a feature?** Find the owner doc in the [Single Source of Truth table](/design/principles/#single-source-of-truth).
- **Reviewing security?** Start at the [](/design/threat-model/) and follow each damage scenario to the owner doc that defeats it.
