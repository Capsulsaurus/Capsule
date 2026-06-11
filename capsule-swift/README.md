# capsule-swift

The native Capsule client for Apple platforms (iPhone & iPad today, macOS
later), written in Swift 6. A local-only, high-performance photo app — the
foundation that Capsule's networked features are wired into later.

## Architecture

A single Tuist project: a thin `Capsule` app target over a graph of framework
modules in `Modules/`.

```text
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

## Running on the iOS Simulator

After `just setup` (or `just setup-swift` from the repo root), pick a simulator
and launch the app from the command line:

```sh
# List available simulators — find the UDID for the device you want
xcrun simctl list devices available

# Boot a simulator (replace the UDID with one from the list above)
xcrun simctl boot "iPhone 16 Pro"

# Open Simulator.app so you can see the screen
open -a Simulator

# Build and install in one step (Debug, simulator)
xcodebuild -workspace Capsule.xcworkspace \
           -scheme Capsule \
           -configuration Debug \
           -destination 'platform=iOS Simulator,name=iPhone 16 Pro' \
           CODE_SIGNING_ALLOWED=NO \
           | mise exec -- xcbeautify

# Install the built .app into the booted simulator and launch it
APP_PATH=$(find ~/Library/Developer/Xcode/DerivedData -name "Capsule.app" \
           -path "*Debug-iphonesimulator*" 2>/dev/null | head -1)
xcrun simctl install booted "$APP_PATH"
xcrun simctl launch booted com.justin13888.capsule.Capsule
```

Or skip the CLI and just press **⌘R** inside Xcode with a simulator destination
selected — it handles build, install, and launch in one action.

## Running on a Physical iPhone

### 1. Configure code signing

Edit `Configuration/Config.xcconfig` and fill in your Apple Developer Team ID:

```text
TEAM_ID=XXXXXXXXXX    # your 10-character team ID from developer.apple.com
```

If you want to change the bundle identifier, edit `BUNDLE_ID` in the same file
(it must match any provisioning profile you create).

### 2. Trust the developer certificate on the device

On first install: **Settings → General → VPN & Device Management → [your Apple
ID] → Trust**. Without this step the app will refuse to launch.

### 3. Build and install over USB (command line)

Plug in the iPhone, unlock it, and trust the Mac if prompted.

```sh
# Find your device UDID
xcrun devicectl list devices

# Build for the real device
xcodebuild -workspace Capsule.xcworkspace \
           -scheme Capsule \
           -configuration Debug \
           -destination 'platform=iOS,id=<device-udid>' \
           | mise exec -- xcbeautify
```

Xcode signs and deploys the app automatically when a valid team is set. After
the build succeeds the app appears on the home screen.

### 4. Wireless install (optional)

Enable **Settings → Privacy & Security → Developer Mode** (iOS 16+) and pair
the device in **Xcode → Window → Devices and Simulators → Connect via network**.
After pairing you can unplug the cable and use the same `xcodebuild` command
with the device UDID — Xcode will push over Wi-Fi.

### 5. Quick iteration from Xcode

Select your iPhone from the scheme selector in the toolbar and press **⌘R**.
Xcode builds, signs, deploys, and attaches the debugger in one step. Use
**⌘⇧<** (Edit Scheme) to switch between Debug and Release builds.
