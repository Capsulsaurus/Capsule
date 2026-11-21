---
title: Supply Chain
description: How a dependency is admitted, how every lockfile is scanned for advisories, how build inputs are pinned, and when an advisory may be ignored.
status: draft
---

This page owns the policy for third-party code and build inputs: admitting a dependency, scanning
lockfiles for known vulnerabilities, pinning what CI executes, and ignoring an advisory. The licence
policy is [](/design/licensing/), and this page does not restate it.

## Gates

| Gate | Tool | Scope | Where it runs |
| --- | --- | --- | --- |
| Licences | cargo-deny (`mise run license-check`) | The Rust graph, every target and feature | `check-rust`, pre-push, CI |
| Rust advisories, bans, sources | cargo-deny (`mise run deny-check`) | `Cargo.lock` against RustSec, unmaintained crates included | CI, every pull request and every push to `master` |
| Advisories in every ecosystem | dependable | `Cargo.lock`, both `bun.lock` files, the Gradle version catalog, each committed `Package.resolved` | CI, every pull request and every push to `master`; pre-push when a manifest or lockfile changes |
| Version updates | Dependabot | One entry per manifest root | Scheduled pull requests, which pass every gate above |

dependable is getkono/dependable: a CLI that checks dependency versions and known vulnerabilities
against the package registries and the OSV database. It needs no API key and has no cloud
backend. It sends package names and versions, which are public in this repository, to those
services and nothing else.

Both advisory gates run whether or not the change touched a dependency. An advisory is published
against a lockfile nobody touched, so a path filter would hide it.

## Advisory Scanning

dependable runs from the repository root over every manifest it discovers. Its version is pinned
in `mise.toml` like every other tool, so local runs and CI agree.

| Lockfile | Ecosystem | What dependable establishes | Gate |
| --- | --- | --- | --- |
| `Cargo.lock` | `rust` | Locked versions and the resolved tree | Reported. cargo-deny is the Rust gate until dependable honours the ignores in `deny.toml` |
| `capsule-web/bun.lock`, `capsule-docs/bun.lock` | `npm` | Locked versions and the resolved tree | `--fail-on vulnerable` |
| `gradle/libs.versions.toml` | `jvm` | Catalog versions only; build scripts are never read | `--fail-on vulnerable` |
| `Package.resolved` beside each `Package.swift` | `swift` | Advisories only (OSV `SwiftURL`); never currency | `--fail-on vulnerable` |

- **Exit 1 fails the gate, and so does exit 2.** Exit 2 means the run could not answer (a registry
  or OSV did not respond). An unanswered gate is never a pass, in CI or in the hook.
- **The pre-push hook runs the same command** as the CI job, through an hk step whose glob covers
  every manifest and lockfile above. It needs the network. Skipping it is an explicit choice for
  one push, never a default.
- **Rust is reported, not gated, by dependable.** `deny.toml` ignores advisories that have no
  reachable fix ([](#advisory-ignores)), and dependable has no ignore list yet, so gating Rust there
  would fail every run on an advisory already accepted. cargo-deny also refuses unmaintained crates,
  which OSV does not report. When dependable can carry the same ignores, Rust joins its gate and
  cargo-deny keeps licences, bans and sources.
- **Every JVM dependency is declared in the catalog.** A coordinate written into a
  `build.gradle.kts` is invisible to dependable, which reports the script as unread. A build script
  references the catalog (`libs.…`) and nothing else.
- **A `Package.resolved` is committed** beside every `Package.swift` that declares a remote package,
  including a Tuist `Tuist/Package.swift`. Without one, dependable reports the package *unknown*: it
  scanned nothing, which reads like a clean result. A package with no remote dependency has nothing
  to pin.
- **Swift is vulnerability-only.** SwiftPM has no registry to ask for newer versions, so Dependabot
  is the only signal that a Swift pin is stale. OSV matches a Swift package by its repository URL as
  `Package.resolved` spells it; write the URL with the casing its forge uses.

## New Dependency Admission

A new dependency is any package that enters a manifest or a lockfile, in any ecosystem and in any
scope, dev and build dependencies included. It is admitted only when all of the following hold, and
the pull request that adds it shows each one.

1. **It earns its place.** It removes code and test scope that Capsule would otherwise own, without
   costing required performance or capability. The pull request names the alternatives it refused,
   including writing the code in-tree. A package already in the lockfile, reached through another
   dependency, is preferred, since it adds no new trust.
2. **dependable is clean.** No open advisory names the admitted version.
3. **cargo-deny passes** for a Rust dependency: its licence is allowed by [](/design/licensing/), it
   is not banned, and it comes from crates.io. A git dependency or another registry needs a reviewed
   `[sources]` entry in `deny.toml` first.
4. **Its maintenance is recorded.** The pull request description names who publishes it, its latest
   release and how recently it changed, and whether more than one person can release it. A package
   with one maintainer and no release in a year needs a stated reason.
5. **Its features are minimal.** Default features are off wherever they enable anything Capsule
   does not use, and every enabled feature is named in the purpose comment.
6. **A purpose comment sits beside the pin.** It says why this package, its bounded scope and
   exceptions, and what was refused. This is the house style of every `Cargo.toml`, of
   `gradle/libs.versions.toml` and of `Package.swift`. `package.json` cannot hold comments, so the
   package's README carries it.
7. **No rule refuses it.** The scoped agent rules name packages that are refused outright; check
   them before proposing one.

A version bump of an admitted dependency is not an admission. It passes the same CI gates, and a
bump that adds a new package to the lockfile admits that package under the rules above.

## Pinned Build Inputs

Everything CI executes or downloads is pinned to content, not to a name that can be moved.

- **Lockfiles are authoritative.** Every `cargo` invocation in CI and in the release workflow runs
  with `--locked`, so a stale `Cargo.lock` fails instead of being rewritten. bun installs with
  `--frozen-lockfile`.
- **Actions are pinned by commit SHA.** Every `uses:` names a full 40-character commit, with the
  release tag in a trailing comment (`# v4.2.2`). Dependabot's `github-actions` ecosystem updates the
  SHA and the comment together. A tag can be moved to other code after review; a commit cannot.
- **Workflows hold least privilege.** The default `permissions` are `contents: read`. A job that
  needs more declares it on the job, never on the workflow.
- **Gradle verifies what it downloads.** `gradle/verification-metadata.xml` records a SHA-256 for
  every artifact and plugin the build resolves, and a mismatch or an unlisted artifact fails the
  build. A dependency change regenerates the file with
  `./gradlew --write-verification-metadata sha256` and the new checksums are reviewed in the diff.
  Dependabot does not regenerate it, so a Gradle update pull request fails verification until a
  maintainer regenerates and pushes the file. That review is the point of the gate.
- **The Gradle wrapper is pinned** by `distributionSha256Sum` in
  `gradle/wrapper/gradle-wrapper.properties`.
- **Toolchains are pinned** by `rust-toolchain.toml` and `mise.toml`.
- **Container base images are pinned by digest** ([](/guides/self-hosting/#container-image)), and
  Dependabot's `docker` ecosystem updates the digest.

## Advisory Ignores

An advisory is fixed by taking a patched version. It is ignored only when no patched version is
reachable, and every ignore carries:

- **the advisory id**, never a severity threshold or a blanket switch;
- **a reason**: the dependency path that brings it in, what blocks the fix, and why the exposure is
  acceptable meanwhile (for example, compile-time only, or a client-side denial of service only);
- **an expiry issue**: an open issue whose resolution removes the entry. When its blocker clears,
  the entry is deleted in the change that takes the fix, and the gate passing proves the advisory
  gone.

The ignore lives in the tool's own configuration next to the advisory: the `[advisories] ignore`
list of `deny.toml` today, whose entries follow this form. An ignore whose issue is closed is a
defect.

## Dependabot

Every manifest root has exactly one Dependabot entry, and every entry points at a directory that
holds its manifest. An entry aimed at a missing manifest is a permanent dashboard error, not an
update, so a change that moves or adds a manifest updates `.github/dependabot.yml` in the same
change.

| Ecosystem | Directory |
| --- | --- |
| `bun` | `/capsule-web`, `/capsule-docs` |
| `cargo` | `/` (the workspace) |
| `docker-compose` | `/capsule-server` |
| `github-actions` | `/` |
| `gradle` | `/` (the version catalog and every included build) |
| `swift` | `/capsule-core-swift` |

## Known Gaps

- dependable is not yet wired into CI, the pre-push hook or `mise.toml`.
- Actions are pinned by tag, not by SHA, in every workflow.
- Neither CI nor the release workflow passes `--locked` to `cargo`.
- There is no Gradle verification metadata, and the wrapper has no `distributionSha256Sum`.
- `capsule-core-kotlin/build.gradle.kts` declares its dependencies as literal coordinates outside
  the catalog, so dependable cannot read them.
- `Package.resolved` is gitignored under `capsule-swift/`.
- The release workflow holds `contents: write` for every job, and publishes no checksums, SBOM or
  build provenance for its binaries.
- No record of third-party code review (such as cargo-vet audits) exists; admission rests on the
  gates above.
