# capsule-swift

The native Capsule client for Apple platforms (iPhone & iPad today, macOS
later), written in Swift 6. A local-only, high-performance photo app — the
foundation that Capsule's networked features are wired into later.

## Architecture

A single Tuist project: a thin `Capsule` app target over a graph of framework
modules in `Modules/`.

```
App/iOS/            thin app target — composition root only
Modules/
  CapsuleFoundation   value types, logging, utilities
  CapsuleCatalog      Swift adapter over the Rust UniFFI catalog
  ManagedStore        Swift filesystem layer + import pipeline
  AssetKit            unified AssetProvider (PhotoKit + managed)
  ImagePipeline       decode / downsample / cache / prefetch
  CapsuleUI           design system + shared components
  FeatureTimeline / FeatureViewer / FeatureAlbums / FeatureSearch
```

The SQLite catalog and CBOR sidecar are owned by Rust (`../capsule-core`) and
exposed to Swift via UniFFI, packaged as `CapsuleCoreFFI.xcframework`. Everything
platform-specific — filesystem, PhotoKit, UI, hashing — is Swift.

## Features

A native, local-only photo experience over a hybrid asset model — the system
Photos library and a Capsule-managed on-disk store merged into one timeline.

- **Timeline** — a `UICollectionView`-backed grid with day sections, pinned
  headers, prefetching, and adjustable density.
- **Viewer** — a paged, zoomable full-screen viewer for photos, Live Photos,
  and video, with an EXIF/location info panel and share / favourite / delete.
- **Import** — bring photos into the Capsule-managed library: SHA-256 hashing,
  content dedup, CBOR sidecars, and a rebuildable SQLite catalog, behind an
  atomic per-item commit.
- **Albums** — system smart albums plus editable Capsule user albums.
- **Search** — filter the unified library by media type and capture date.

### Known limitations (prototype scope)

- Import is photo-only; video import and Live Photo stacking are not yet wired.
- The grid materialises the timeline; the fully-lazy large-library path,
  GPS→timezone resolution, and the iOS 18 cell→viewer zoom transition are
  future work.
- Capsule's networked features (API, sync, E2E encryption) are out of scope.

## Development

Prerequisites: macOS, Xcode 16+, a Rust toolchain (pinned by
`/rust-toolchain.toml`), and [mise](https://mise.jdx.dev).

```sh
cd capsule-swift
mise install            # installs tuist, swiftlint, swiftformat, xcbeautify
just setup              # builds the Rust FFI xcframework, then `tuist generate`
open Capsule.xcworkspace
```

`just setup` is equivalent to:

```sh
just build-ffi-apple            # cross-compiles capsule-core-ffi → .ffi/CapsuleCoreFFI.xcframework
mise exec -- tuist generate     # generates Capsule.xcworkspace
```

Re-run `just build-ffi-apple` whenever the Rust core changes. The generated
Xcode project/workspace and the `.ffi/` build output are not committed.
