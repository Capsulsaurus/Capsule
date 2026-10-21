# Capsule

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="images/readme-hero-dark.png">
    <img src="images/readme-hero-light.png" alt="Capsule on iPhone: the Library timeline showing a grid of photos" width="360">
  </picture>
</p>

Open-source, federated, end-to-end encrypted photo library for photographers and prosumers.

## Goals

- **Your library outlives any service.** Originals are stored permanently, at full quality, with their metadata, and can always be restored byte for byte.
- **End-to-end cryptography.** The server stores only ciphertext. Every asset carries a signed provenance chain, so who captured, imported and edited it can be verified rather than trusted.
- **Federated sharing.** Because trust lives in the cryptography rather than in a server, people on different servers can share albums with each other: think [Matrix](https://en.wikipedia.org/wiki/Matrix_(protocol)), but for images.
- **Open source for the network effect.** An open protocol and self-hostable server let anyone join the network; the aim is to be the first open, federated standard for personal photo libraries.
- **Native everywhere.** Each platform gets a real native client; the CLI/TUI is the reference client and stays the most robust one.

## Status

Alpha, and not yet ready for data you cannot afford to lose. The current focus is the first dogfoodable release: the Rust CLI/TUI imports, the server stores permanently, and every step of that path is covered by automated tests. Native apps follow once that path is solid. The [roadmap](ROADMAP.md) gives the ordered path.

## Getting started

- [Getting started](https://capsule.justinchung.net/guides/getting-started/) and the [user guide](https://capsule.justinchung.net/guides/user-guide/).
- [Self-hosting](https://capsule.justinchung.net/guides/self-hosting/) a server.
- [Design docs](https://capsule.justinchung.net/design/) for how it works.

## Repository

| Path | What it is |
| --- | --- |
| `capsule-core` | Rust core: cryptography, media, metadata, local library |
| `capsule-sdk` | Rust client SDK over the generated API client |
| `capsule-cli` | The `capsule` CLI and TUI |
| `capsule-server` | The server (Kynos REST/OpenAPI) |
| `capsule-e2e` | End-to-end tests across the real binaries |
| `capsule-swift`, `capsule-android` | Native Apple and Android clients |
| `capsule-web` | Read-only web client |
| `capsule-docs` | Documentation site |

Install [mise](https://mise.jdx.dev), run `mise install` and `mise run hooks-install`, then see [CONTRIBUTING.md](CONTRIBUTING.md). Report bugs in GitHub Issues; ask setup questions in GitHub Discussions.

## Contributing and licence

Contributions need a signed [CLA](CLA.md): a single copyright holder is what keeps app-store distribution possible, and every public release stays under an OSI-approved licence.

Capsule is published under [AGPL-3.0-only](LICENSE); third-party notices are in [NOTICE](NOTICE). The copyright holder, Justin Chung, may also offer it under separate terms, such as those app stores require; any version you receive under the AGPL stays yours under the AGPL. See the [licensing design doc](https://capsule.justinchung.net/design/licensing/).
