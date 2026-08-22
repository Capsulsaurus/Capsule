# capsule-swift

The native Capsule client for Apple platforms — **iPhone, iPad, and Mac from one
codebase**, written in Swift 6. macOS is a native destination, not Catalyst: the
Mac build gets a real `NavigationSplitView`, menu-bar commands, a `Settings`
scene, and multiple windows.

The deployment floor is **iOS/iPadOS 26 and macOS 26**. That is deliberate: it
makes the Liquid Glass design system, `.tabBarMinimizeBehavior`, and
`.navigationTransition(.zoom)` available unconditionally, so no UI code carries an
`#available` fence and there is one visual language to design and audit.

## Two lanes

The app builds in one of two configurations, selected by a Tuist environment flag.

| Lane | How | What backs the data |
| --- | --- | --- |
| **Mock** (default) | `mise run setup-swift` | `CapsuleMock` — in-memory adapters for every port, with a scenario switcher that drives edge states (quarantine, quota grace, degraded federation, awaiting-originals…) |
| **FFI** | `TUIST_FFI=1`, after `mise run setup-swift-ffi` | The Rust core over UniFFI: `CapsuleCatalogFFI` compiles the generated glue and links `CapsuleCoreFFI.xcframework` |

The mock lane needs **no Rust toolchain, no cross-compile, and no `.ffi/`
directory** — `tuist generate && xcodebuild test` works from a clean checkout with
only Xcode installed. That is what keeps the UI lane fast to build and verify
while `capsule-core` and `capsule-sdk` are being rebuilt.

Both lanes compile against the same protocols in `CapsulePorts`, so a signature
change breaks both at compile time rather than letting the FFI lane rot.

## Architecture

A single Tuist project: a thin `Capsule` app target over a graph of framework
modules in `Modules/`.

```text
App/                 thin app target — composition root only
Modules/
  CapsuleFoundation   value types, logging, utilities, Platform/ (the shim)
  CapsuleDomain       display/domain models mirroring the future uniffi records
  CapsulePorts        the async protocol seams — one per capability
  CapsuleMock         in-memory adapters, deterministic fixtures, scenarios
  CapsuleCatalog      the catalog contract, models, and in-memory reference impl
  CapsuleCatalogFFI   the Rust-backed half (FFI lane only)
  ManagedStore        Swift filesystem layer + import pipeline
  AssetKit            unified AssetProvider (PhotoKit + managed)
  ImagePipeline       decode / downsample / cache / prefetch
  CapsuleUI           design system, shared components, PlatformCollection/
  CapsuleNavigation   routes, split-view state, deep links, macOS commands
  Feature*            one module per screen group
```

### The platform rule

`import UIKit` and `import AppKit` appear in exactly two places:
`CapsuleFoundation/Sources/Platform/` (the type shim — `PlatformImage`,
`PlatformColor`, `PlatformEnvironment`, `PlatformLifecycle`) and
`CapsuleUI/Sources/PlatformCollection/` (the collection-view island that backs
every grid). Everything else is SwiftUI written once. Feature code branches on
*capability* (`PlatformEnvironment.hasMenuBar`) rather than on `#if os(...)`, so a
future destination is a matter of extending the shim, not auditing every view.

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

Prerequisites: macOS, Xcode 26+, and [mise](https://mise.jdx.dev). **No Rust
toolchain is required for the default lane.**

```sh
cd capsule-swift
mise install            # installs tuist, swiftlint, swiftformat, xcbeautify
mise run setup-swift    # `tuist generate` — that is all the mock lane needs
open Capsule.xcworkspace
```

To work on the Rust-backed half instead:

```sh
mise run setup-swift-ffi   # build-ffi-apple, then TUIST_FFI=1 tuist generate
```

### Checks

```sh
mise run check-swift    # format + lint + tests on macOS, iOS, and iPadOS
```

`check-swift` is the CI gate and maps 1:1 to the `swift` job in `ci.yml`. The
Rust-backed lane has its own workflow, `build-ios.yml`.

Re-run `mise run build-ffi-apple` whenever the Rust core changes. The generated
Xcode project/workspace and the `.ffi/` build output are not committed.

## Running on the Mac

The Mac build needs no simulator at all — it is the fastest way to see a change:

```sh
xcodebuild -workspace Capsule.xcworkspace -scheme Capsule \
           -configuration Debug -destination 'platform=macOS' \
           CODE_SIGNING_ALLOWED=NO build | mise exec -- xcbeautify

open "$(find ~/Library/Developer/Xcode/DerivedData -name 'Capsule.app' \
        -path '*Debug*' -not -path '*iphonesimulator*' | head -1)"
```

Or select **My Mac** in the scheme selector and press **⌘R**.

## Running on the iOS Simulator

After `mise run setup-swift`, pick a simulator and launch the app from the
command line. The app needs an iOS 26 runtime; if `xcodebuild` reports no
simulator destinations, run `xcodebuild -downloadPlatform iOS` (no sudo).

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
