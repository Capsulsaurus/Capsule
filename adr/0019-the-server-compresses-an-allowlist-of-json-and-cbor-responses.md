# ADR-0019 — The server compresses an allowlist of JSON and CBOR responses, and nothing else does

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Network Resilience — Compression](../capsule-docs/src/content/docs/design/networking.md#compression)
- **Issues:** —

## Context

Nothing on the wire was compressed. The server's Kynos dependency enabled only `openapi32`
(`Cargo.toml`, the workspace `kynos` pin), and the SDK's reqwest enabled no decoder
(`capsule-sdk/Cargo.toml`, the `reqwest` features). No document said whether compression was
wanted, who would apply it, or what it must never touch. Any of three parties could have applied it:
the server, a reverse proxy in front of it, or nobody.

Most bytes Capsule moves are ciphertext, and ciphertext does not compress. What does compress is
the JSON around it. The sync feed is the clearest case: repeated keys, and base64 fields of which
a quarter of every character is redundant. A phone on a metered path reads that feed more than
anything else.

Kynos ships response compression behind a `compression` feature. It negotiates zstd, brotli and
gzip, skips bodies below a size threshold, leaves ranged and strongly-validated responses
alone, and lets a handler disable encoding for one response (its documentation names the BREACH
case). It declares `Vary: Accept-Encoding` on every response it covers, encoded or not. It does
not choose routes by media type.

## Decision

The server decides, per operation. It mounts Kynos's compression only on an explicit allowlist of
JSON and CBOR operations that are lists or feeds, above a size threshold. It never compresses
`application/octet-stream`, and never an operation whose response carries a secret beside input
an attacker can influence. Proxies are pass-through and do not add or strip codings. SDK clients
accept zstd and gzip; a browser negotiates its own codings. No client compresses a request body,
and every client bounds every body after decoding.

## Consequences

- **The allowlist is the safety property.** Default-on compression with per-route opt-outs was
  rejected. A new route that returns a token would be compressed until someone remembered to
  exclude it, and a BREACH oracle does not announce itself. Under an allowlist, forgetting
  costs bandwidth, never a secret.
- **A proxy that compresses defeats the exclusions.** nginx's `gzip on` does not know which
  operations carry tokens. The self-hosting guide therefore has to say *off* for Capsule's
  upstream, and the doc states proxies as pass-through rather than leaving it to taste.
- **Brotli is not offered by SDK clients.** zstd matches or beats it on these bodies at a lower
  CPU cost, and gzip is the universal fallback. A third decoder in the SDK would buy nothing. A
  browser sets `Accept-Encoding` itself and a page cannot change it, so the web client takes
  whatever the browser negotiates, brotli included, and still caps the decoded body.
- **Decoded-size caps become mandatory.** A compressed body is a decompression-bomb vector
  against a phone, and the server is outside the client's trust. The cap applies after
  decoding, so it also bounds an uncompressed oversized body, which was unbounded before this
  decision too.
- **Dependency work.** The server enables Kynos's `compression` feature, and the SDK's backend
  enables its zstd and gzip decoders. Caches need nothing extra: Kynos already sends
  `Vary: Accept-Encoding`.
- **No request compression.** Upload bodies are ciphertext, and the JSON requests are small.
  Request compression would add a decoder to the server's attack surface for no gain.
