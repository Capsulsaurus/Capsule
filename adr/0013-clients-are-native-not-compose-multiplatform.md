# ADR-0013 — Each client is written natively, and shared logic reaches it through Rust, not a cross-platform UI framework

- **Status:** accepted
- **Date:** 2026-10-29
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Clients — Design Priorities](../capsule-docs/src/content/docs/design/clients.md#design-priorities)
- **Issues:** #992

## Context

Capsule ships a client on iOS, iPadOS, macOS and Android, and Windows and Linux later. Writing
each one natively costs a second UI codebase per platform. A cross-platform UI framework would
remove that cost, so the question has been reopened every year, with Compose Multiplatform as the
candidate each time.

Two things have kept the answer the same:

- **Compose Multiplatform's desktop target still runs on the JVM.** That target is what
  Windows and Linux would use, and it still needs a JVM runtime after several years. A JVM in a
  desktop photo app costs startup time, memory and a runtime to bundle. That is the same class of
  cost the project avoids by keeping the core in Rust.
- **Kotlin's Swift export took too long to stabilise.** Without it, Kotlin shared with Apple
  clients reaches Swift through an Objective-C header, which loses much of Swift's type
  fidelity. Capsule already gets generated Swift types from Rust through UniFFI
  ([ADR-0005](0005-one-uniffi-namespace-pair-reaches-the-apps.md)).

The logic worth sharing (crypto, import, sync, transfer, the library) already lives in Rust and
reaches every app through one FFI pair. A shared UI layer would sit on top of that, and the only
thing it would share is the views. But the [design language](../capsule-docs/src/content/docs/design/clients.md#design-language)
deliberately does not share views: HIG on Apple platforms and Material on Android.

## Decision

Apple clients are SwiftUI and Android is Jetpack Compose, each over the Rust core through the
UniFFI pair ADR-0005 fixes. No cross-platform UI framework sits between Rust and a client. Code
shared between clients is Rust; code shared between Apple clients is Swift.

## Considered and rejected

- **Compose Multiplatform** for Android, desktop and possibly iOS. It needs a JVM on Windows and
  Linux, and its iOS rendering is not UIKit, so it would emulate HIG rather than use it.
- **Kotlin Multiplatform for shared logic, with Swift export.** It duplicates what Rust already
  shares, and Swift export was not stable when this decision was needed.
- **Flutter.** Its own rendering engine and widget set would be the "third house style" the
  design language rules out, and it would add a Dart layer between the apps and Rust.

## Consequences

- Every feature is built once per design language. This is the cost the minimal-divergence
  priority in the contract keeps small: client code is mostly display.
- The Android module still applies the Kotlin Multiplatform and Compose Multiplatform Gradle
  plugins of the starter template it began from (`capsule-android/README.md`). It targets
  Android only, so they add build surface and buy nothing. They are to be replaced with the
  Android Gradle plugin and Jetpack Compose.
- Windows and Linux are deferred. This decision rules out reaching them through Compose
  Multiplatform and leaves their toolkit to be chosen when they are built, under the same rule.
- This decision is reopened only if Compose Multiplatform's desktop target drops the JVM. If it
  is, the replacement ADR supersedes this one.
