---
title: Self Hosting
description: Current self-hosting status and planned deployment dependencies
---

The previous server has been removed from the active workspace while its Kynos replacement is being
designed. There is currently no supported Capsule server deployment. Sources under
`legacy-review/` are not deployable.

## Planned Profile

The supported server will expose one REST/OpenAPI API and require:

- PostgreSQL for the authoritative key-free index and default upload-session state.
- A Capsule-owned filesystem blob store for opaque content-addressed ciphertext.
- Valkey only for the measured high-concurrency profile; it is not a default requirement.
- A TLS-terminating ingress or Kynos-native TLS configuration appropriate to the deployment.

Clients perform media processing, metadata extraction, derivative generation, encryption, signing,
and cryptographic verification. The server never decodes uploaded media.

Deployment instructions will be published only after the Kynos server, migrations, storage
verification, backup/restore, readiness, graceful shutdown, and upgrade tests are complete.
