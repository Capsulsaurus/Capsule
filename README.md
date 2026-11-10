<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="images/logo-dark.svg">
    <img src="images/logo-light.svg" alt="Capsule" width="280">
  </picture>
</p>

<!-- languages:begin -->

<p align="center"><a href="README.md">English</a> · <a href="README.zh-Hans.md">简体中文</a> · <a href="README.zh-Hant.md">繁體中文</a> · <a href="README.ja.md">日本語</a> · <a href="README.ko.md">한국어</a> · <a href="README.fr.md">Français</a> · <a href="README.de.md">Deutsch</a> · <a href="README.es.md">Español</a> · <a href="README.pt-BR.md">Português (Brasil)</a> · <a href="README.it.md">Italiano</a> · <a href="README.ru.md">Русский</a> · <a href="README.hi.md">हिन्दी</a> · <a href="README.ar.md">العربية</a></p>

<!-- languages:end -->

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/licence-AGPL--3.0-blue" alt="Licence: AGPL-3.0"></a>
  <a href="https://github.com/Capsulsaurus/Capsule/actions/workflows/ci.yml"><img src="https://github.com/Capsulsaurus/Capsule/actions/workflows/ci.yml/badge.svg?branch=master" alt="CI"></a>
  <a href="https://github.com/Capsulsaurus/Capsule/discussions"><img src="https://img.shields.io/github/discussions/Capsulsaurus/Capsule" alt="Discussions"></a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="images/readme-hero-dark.png">
    <img src="images/readme-hero-light.png" alt="Capsule on iPhone: the Library timeline showing a grid of photos" width="360">
  </picture>
</p>

**A photo library that organizes millions of photos on its own, that no server can see into, and that shares across servers the way email does.** Capsule is open source and end-to-end encrypted, built for people who shoot thousands of pictures a year, photographers whose RAW files are their livelihood, travellers bringing the world home on a memory card, and families keeping decades of memories. Run it on your own server or a friend's, and share with anyone, on any server.

> [!NOTE]
> **Alpha.** Mobile clients are being stabilized and refined prior to distribution to beta testers. Follow along on the [roadmap](ROADMAP.md).

## Why Capsule

- **Organize millions of photos, systematically and automatically.** Every import lands on one timeline, RAW+JPEG pairs and bursts\* are stacked, duplicates are caught by their content, and smart albums keep themselves up to date. A library of millions stays as easy to navigate as one of hundreds.
- **Private by construction, not by promise.** Photos are encrypted on your devices before they leave. The server stores only ciphertext and never holds a key, so not even whoever runs it can look.
- **Proof of where every photo came from.** Each photo carries a signed history of who captured, imported and edited it, so its origin can be verified rather than trusted.
- **Share across servers.** People on different servers share albums with each other, the way email works between providers. Nobody has to join your server, and no company sits in the middle.
- **Your whole library, even offline.** Every app keeps an index of everything you have ever kept, so you can browse, search and organize all of it on a plane, at the border, or far from any signal.
- **Straight from the camera.** Pull photos over Wi-Fi or cable directly from your camera, with every file checksummed on arrival.
- **A library that outlives Capsule.** Every photo sits beside a signed sidecar in an open format, so your library stays readable without the app, without the database, and without us.
- **Native on every device\*.** Real native apps that follow each platform's own design, a web viewer for anyone you share with, and a scriptable command line and terminal UI.

\* Coming later, prioritized by demand.

## Everything you'd expect

- Albums, plus your originals kept byte for byte with their metadata and restorable exactly as they went in
- Search by date, place, camera, lens, rating and label, run on your device
- Ratings, picks and rejects for culling a shoot; labels and captions
- A map of where every photo was taken, with location fixes that keep the original
- Share links anyone can open in a browser, and upload links that let guests add their photos to yours
- Backups that resume where they stopped, and device-to-device sync over your local network
- Trash with a recovery window, hidden photos, and account recovery if you lose a device
- Apps in 13 languages

## On the roadmap

- **On-device AI.** Search in plain words ("sunset over the harbour"), find people, pets and places, and surface your best shots, all computed on your devices. AI suggestions are kept apart from your own labels and never overwrite them.
- **Built-in, format-aware image compression.** Choose lossless or lossy compression for each format, so a library takes less space on exactly your terms.
- **More importers.** One-step moves from iCloud Photos and Immich, alongside Google Photos.
- **Windows and Linux apps**, after the phone, tablet and Mac apps.

## Get started

- **Try it.** The [getting-started guide](https://capsule.justinchung.net/guides/getting-started/) sets up a local library on your computer; no server is needed.
- **Bring your library.** Import folders, drives and memory cards, pull from a camera, or [move from Google Photos](https://capsule.justinchung.net/guides/google-photos-migration/).
- **Run a server** for yourself, your family or your friends: [self-hosting](https://capsule.justinchung.net/guides/self-hosting/).
- **See how it works** in the [design docs](https://capsule.justinchung.net/design/).

## Translations

<!-- translations:begin -->

| Language | README | App strings translated | Reviewed by a fluent reader |
| --- | --- | --- | --- |
| 简体中文 | [README.zh-Hans.md](README.zh-Hans.md) | ![zh-Hans translated](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27zh-Hans%27%5D.translated&suffix=%25&label=&color=blue) | ![zh-Hans reviewed](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27zh-Hans%27%5D.reviewed&suffix=%25&label=&color=blue) |
| 繁體中文 | [README.zh-Hant.md](README.zh-Hant.md) | ![zh-Hant translated](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27zh-Hant%27%5D.translated&suffix=%25&label=&color=blue) | ![zh-Hant reviewed](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27zh-Hant%27%5D.reviewed&suffix=%25&label=&color=blue) |
| 日本語 | [README.ja.md](README.ja.md) | ![ja translated](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27ja%27%5D.translated&suffix=%25&label=&color=blue) | ![ja reviewed](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27ja%27%5D.reviewed&suffix=%25&label=&color=blue) |
| 한국어 | [README.ko.md](README.ko.md) | ![ko translated](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27ko%27%5D.translated&suffix=%25&label=&color=blue) | ![ko reviewed](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27ko%27%5D.reviewed&suffix=%25&label=&color=blue) |
| Français | [README.fr.md](README.fr.md) | ![fr translated](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27fr%27%5D.translated&suffix=%25&label=&color=blue) | ![fr reviewed](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27fr%27%5D.reviewed&suffix=%25&label=&color=blue) |
| Deutsch | [README.de.md](README.de.md) | ![de translated](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27de%27%5D.translated&suffix=%25&label=&color=blue) | ![de reviewed](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27de%27%5D.reviewed&suffix=%25&label=&color=blue) |
| Español | [README.es.md](README.es.md) | ![es translated](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27es%27%5D.translated&suffix=%25&label=&color=blue) | ![es reviewed](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27es%27%5D.reviewed&suffix=%25&label=&color=blue) |
| Português (Brasil) | [README.pt-BR.md](README.pt-BR.md) | ![pt-BR translated](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27pt-BR%27%5D.translated&suffix=%25&label=&color=blue) | ![pt-BR reviewed](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27pt-BR%27%5D.reviewed&suffix=%25&label=&color=blue) |
| Italiano | [README.it.md](README.it.md) | ![it translated](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27it%27%5D.translated&suffix=%25&label=&color=blue) | ![it reviewed](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27it%27%5D.reviewed&suffix=%25&label=&color=blue) |
| Русский | [README.ru.md](README.ru.md) | ![ru translated](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27ru%27%5D.translated&suffix=%25&label=&color=blue) | ![ru reviewed](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27ru%27%5D.reviewed&suffix=%25&label=&color=blue) |
| हिन्दी | [README.hi.md](README.hi.md) | ![hi translated](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27hi%27%5D.translated&suffix=%25&label=&color=blue) | ![hi reviewed](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27hi%27%5D.reviewed&suffix=%25&label=&color=blue) |
| العربية | [README.ar.md](README.ar.md) | ![ar translated](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27ar%27%5D.translated&suffix=%25&label=&color=blue) | ![ar reviewed](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json&query=%24%5B%27ar%27%5D.reviewed&suffix=%25&label=&color=blue) |

<!-- translations:end -->

Translations start as machine drafts and are marked reviewed once a fluent reader accepts them. To help, see [locales/README.md](locales/README.md), [request a language](https://github.com/Capsulsaurus/Capsule/issues/new?template=language-request.md) or [report a translation error](https://github.com/Capsulsaurus/Capsule/issues/new?template=translation-error.md).

## Contributing

Install [mise](https://mise.jdx.dev), run `mise install` and `mise run hooks-install`, then see [CONTRIBUTING.md](CONTRIBUTING.md). Report bugs in GitHub Issues; ask setup questions in GitHub Discussions. Contributions need a signed [CLA](CLA.md): a single copyright holder is what keeps app-store distribution possible, and every public release stays under an OSI-approved licence.

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

## Licence

Capsule is published under [AGPL-3.0-only](LICENSE); third-party notices are in [NOTICE](NOTICE). The copyright holder, Justin Chung, may also offer it under separate terms, such as those app stores require; any version you receive under the AGPL stays yours under the AGPL. See the [licensing design doc](https://capsule.justinchung.net/design/licensing/).
