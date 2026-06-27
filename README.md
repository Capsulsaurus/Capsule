# Capsule

Open-source, federated, E2E encrypted photo management and sharing service built for professionals and prosumers.

> Disclaimer: This project continues to be in active development. Star this repo to get the latest updates!
>
> Alpha status: The `master` branch is the main development branch. Releases on GitHub will be made once: 1. Features/APIs have been stabilized internally; 2. Internal testing group have tested sufficiently.
>
> Project Update (April 2026): We are porting to E2E encryption and offline-first philosophy so it will be another few months before we reach alpha again.
> Project Update (June 2026): Developer is still working actively but only during spare time. Lots of new prerequisite work on establishing portable, pure Rust libraries in separate repos have been blocking (e.g. image encoding/decoding, metadata processing, geocoordinate logic, on-device ML models finetuned for use cases). However, fleshing out these missing building blocks in the Rust ecosystem is paramount to a solid product.

## Features

- **Native and cross-platform**: Capsule is available on all common desktop and mobile platforms. They are fast and native on all.
- **Broadest format support**: Capsule supports the majority of image and video formats from ones in common smartphones to professional RAW formats. View any content on any device just like your smartphone photos and videos!
- **Data is always optimized**: Capsule rigorously caches and optimizes any heavy content for seamless delivery (our methodology is [documented](https://capsule.justinchung.net/design/thumbnails/)). If something feels slow, let us know.
- **Privacy**: Your data is yours and end-to-end encrypted.
- **Fully-featured access**: Capsule implements several powerful features like real-time viewing, semantic search, AI organization, and more.
- **Federated**: Capsule users can share their assets with users of other servers seamlessly. Think of it like [Matrix](https://en.wikipedia.org/wiki/Matrix_(protocol)) for photos.
- **Open-source**: Capsule is open-source forever and you can host your own server.

## Is Capsule for you?

Capsule is highly refined for photographers and prosumers who want to store and share their photos nearly as seamlessly as do cloud photo services that do not necessarily work on all your devices equally as well and own your data.

We implement strict security and privacy requirements with the assumption that any data stored can be viewed by unauthorized parties. As such, everything is end-to-end encrypted and processed locally.

However, it is important to note that (at least currently) Capsule requires a **self-hosted** server which requires some technical knowledge. This is not a turn-key solution but rather a capable and actively-developed open-source project. It was created out of passion and so I (as the author) do not ask for any monetary compensation. The best form of compensation is technical contributions and feedback!

## Screenshots

<!-- TODO: Add screenshots -->

## Who is Capsule for?

- **Photographers**: Capsule is designed for photographers who want to store and share their photos with clients and peers.
- **Families and friends**: Capsule is designed for prosumers who want to store and share their photos and videos with each other at full quality, complete with metadata.
- **Organizations**: Organizations can use Capsule to share full-quality photos and videos with their members and clients.
- **People who care about privacy**: Capsule implements the best of privacy and security practices and leaves the data in your hands.

### Who is Capsule not for?

This is a personal choice but if you're happy with existing services like Google Photos or iCloud, or sending highly compressed content over messaging apps, Capsule might not be for you.

## Some similar alternatives

- **Google Photos or similar**: Google Photos is a great service for storing and sharing photos and videos. However, it compresses and strips metadata from your photos and videos by default, and does not support many more professional or non-smartphone formats.
- **AirDrop, Quick Share or some messaging app**: These options are great for sharing photos and videos quickly, but they compress the content, have size limits, and/or do not store it for long-term access. If you have more than a few gigabytes of content, Capsule should offer a much more comfortable experience.

## Getting Started

[See our docs](https://capsule.justinchung.net/guides/getting-started/).

### Features Relevant to New Users

*Note: This section is being updated on the development branch.*

- Official tools for bulk imports: Capsule was built to support massive (TB+) imports on the fly. It handles deduplication, bulk organization, etc. for you. (For existing libraries, it is recommended that you use the CLI though.)
- Native clients with native features: Sometimes you want to backup your content on particular platforms that require platform-specific APIs (e.g. iOS). If we have a native client for you, it will be using the latest APIs available in that ecosystem.
- Performance-oriented architecture: Capsule is focused on providing a robust and secure backend to store your most important data, and you can be sure whatever big collection you have will be processed with all the hardware you give.

## Development

<!-- TODO: Add complete architecture diagram -->

Components:

- [Capsule API](capsule-api/README.md): Various API services (HTTP, gRPC, GraphQL, WebSockets, etc.)
- [Capsule Web](capsule-web/README.md) (WIP): Web client in React
- [Capsule Core Kotlin](capsule-core-kotlin/README.md): Shared core Kotlin multiplatform library for client-specific logic
- [Capsule Desktop](capsule-desktop/README.md) (Planned): Windows/Linux desktop client
- [Capsule Android](capsule-android/README.md) (WIP): Jetpack Compose App
- [Capsule Swift](capsule-swift/README.md): SwiftUI client for iOS/macOS
- [Capsule Media](capsule-media/README.md) (Beta): C++ library for certain offloading
- [Capsule Docs](capsule-docs/README.md): Documentation website in Starlight (Astro)

<!-- TODO: ensure readme links work ^^ -->
<!-- TODO: TO be updated ^^ -->

External dependencies:

- [PostgreSQL](https://www.postgresql.org/)
- [MinIO](https://min.io/)
- [RabbitMQ](https://www.rabbitmq.com/)
- [Memcached](https://memcached.org/)

- [Envoy](https://github.com/envoyproxy/envoy)
- [Istio](https://github.com/istio/istio)

<!-- TODO: To be updated ^^ -->

Considering all the technologies used, you may have to switch between IDEs to develop various parts of the project. This is what we recommend:

- `capsule-android`: Android Studio or IntelliJ IDEA with plugins
- `capsule-api`: VS Code or similar
- `capsule-core-kotlin`: Android Studio or IntelliJ IDEA with plugins
- `capsule-desktop`: VS Code or similar
- `capsule-docs`: VS Code or similar
- `capsule-media`: VS Code or similar
- `capsule-swift`: Xcode
- `capsule-web`: VS Code or similar

<!-- TODO: Update list of components ^^ -->

### Setup

This is a polyglot monorepo (5+ programming languages: Rust, TypeScript, Kotlin, Swift/Objective-C, C/C++, Python), so each language uses its native toolchain rather than a single unified build system. [mise](https://mise.jdx.dev) pins the shared dev tooling (`just`, `lefthook`, `convco`) and [just](https://just.systems) is the task runner that ties the per-language tasks together (`just check`, `just build`, etc.). Various tools will need to be setup based on services you need to work on.

Setup in the following order:

- Install [mise](https://mise.jdx.dev) and run `mise install` from the repo root to fetch the pinned shared tooling.
- Install the git hooks with `just hooks-install`.
- Setup all necessary tools related to Kotlin Multiplatform: <https://www.jetbrains.com/help/kotlin-multiplatform-dev/multiplatform-setup.html>
- Setup each of the following tools in the Development sections of each component's README.

### Style and Guidelines

- Due to the numerous languages in this monorepo, we use multiple linters/formatters, each native to each language/technology. CI/CD will enforce these and it is recommended to use the same tools in the IDE of your choice to reduce merge conflicts. (Also, all code is standardized to 4 spaces as some languages have specific guidelines while others (e.g. TypeScript) have mixed guides.)

<!-- TODO: Add internationalization note -->

## Issue Reporting

GitHub Issues is the only accepted method of technical issue reporting. For assistance on setup, we recommend opening a GitHub Discussion (for visibility).

> Note on third-party clients and scripts: This is outside the scope of the project but if there is missing functionality that led to resorting to external solutions, we highly encourage you to submit a feature request!

## FAQ

**Q: Why may Capsule be more suitable than other open-source solutions?**

A: Capsule is designed from the ground up with performance, usability, and compatibility in mind. While hosting requires some initial setup (all of which is carefully documented), we have by far the most comprehensive format support, real-time viewing capabilities. We thoroughly test the supported hardware and software combinations and conservatively push new features to stable. It should be a great option for those with large amounts of content and want a single pane of glass to manage all their assets from any device.

**Q: Why not extend off existing open-source solutions?**

A: While there are multiple great open-source solutions, they lack a lot of the involved functions that professionals and prosumers need. For prosumers interested in an open-source and self-hosted solution, we have a robust, and highly scalable solution. For professionals looking to host all their assets in a seamless and integrated service, we have a solution that may be a better fit than some proprietary options.

Side note: The original author loves open-source and has contributed to various projects. The reason for starting from the ground up is that many of the technical decisions to achieve the goals with user experience and performance require multiple critical design decisions.
**Q: For the API, were languages other than Rust considered?**

A: Yes, we considered many languages. Some other languages considered included Go, TypeScript, Kotlin/Java. In fact, the first PoC was as a single REST API written in TypeScript. However, the current development has developed into multiple APIs (GraphQL, REST, gRPC) and processing logic offloaded to clients of various platforms. Rust offers both the memory-safety and performance requirements, as well as the cross-platform flexiblity that some other languages may equally excel at. On the APIs, Rust libraries also tend to be newer and allowed for Linux-specific optimizations such as using `io_uring` for high-performance async I/O. Additonally, note that several other languages with other strengths are embraced.

**Q: How do bugfixes happen?**

A: Even if the best development practices, rigorous testing, and conservative designs, no software is without bugs. Bug reporting and thoughtful feature requests submitted to the issues page would be much appreciated. Certain types of bugs, such as those affecting data integrity, should be marked with the appropriate tags and will be ironed out ASAP. Patches typically roll out to the latest major version and marked in a notice board separate from CHANGELOGs.

## How to contribute

Capsule primarily benefits from active contributions and feedback! Rather than a donation we actually need more hands. See [CONTRIBUTING.md](./CONTRIBUTING.md) for details.

### Contributor License Agreement (CLA)

We require all contributors to sign a CLA. This ensures the project maintains the legal flexibility necessary to secure future funding and a dedicated maintenance team, guaranteeing the software remains actively supported and self-sufficient for decades to come.

## License

Capsule is licensed under the [AGPL-3.0 License](LICENSE).
