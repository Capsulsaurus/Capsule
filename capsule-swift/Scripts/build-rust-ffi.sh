#!/usr/bin/env bash
#
# Build capsule-core-ffi for Apple platforms and package it as an xcframework
# alongside the generated Swift bindings. Outputs land in capsule-swift/.ffi/
# (git-ignored) and are consumed by the Tuist `CapsuleCatalog` module.
#
# The simulator slice is a universal binary (arm64 + x86_64) so the xcframework
# links on both Apple Silicon and Intel Macs.
#
# Run via `mise run build-ffi-apple`, or directly. Requires: rustup, cargo, xcodebuild.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SWIFT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$SWIFT_DIR/.." && pwd)"

CRATE="capsule-core-ffi"
LIB_NAME="libcapsule_core_ffi.a"
DEVICE_TARGET="aarch64-apple-ios"
SIM_ARM_TARGET="aarch64-apple-ios-sim"
SIM_X86_TARGET="x86_64-apple-ios"

FFI_OUT="$SWIFT_DIR/.ffi"
GEN_DIR="$FFI_OUT/generated"
HEADERS_DIR="$FFI_OUT/headers"
BUILD_DIR="$FFI_OUT/build"
XCFRAMEWORK="$FFI_OUT/CapsuleCoreFFI.xcframework"

cd "$REPO_ROOT"

echo "▸ Ensuring Rust Apple targets are installed"
rustup target add "$DEVICE_TARGET" "$SIM_ARM_TARGET" "$SIM_X86_TARGET" >/dev/null

for target in "$DEVICE_TARGET" "$SIM_ARM_TARGET" "$SIM_X86_TARGET"; do
    echo "▸ Building $CRATE staticlib for $target"
    cargo build -p "$CRATE" --lib --release --target "$target"
done

echo "▸ Generating Swift bindings"
rm -rf "$GEN_DIR"
mkdir -p "$GEN_DIR"
cargo run -p "$CRATE" --bin uniffi-bindgen -- \
    generate --library "$REPO_ROOT/target/$DEVICE_TARGET/release/$LIB_NAME" \
    --language swift --out-dir "$GEN_DIR"

# The staticlib is the app umbrella (S-F3): it carries the capsule_core_ffi namespace
# AND capsule-sdk's capsule_sdk namespace (S-D9), so library-mode bindgen must have
# emitted Swift glue for both. Fail loudly if either went missing.
test -s "$GEN_DIR/capsule_core_ffi.swift"
test -s "$GEN_DIR/capsule_sdk.swift"

# Swift 6 language mode (SWIFT_VERSION 6.0 in Project.swift) rejects a global of a
# non-Sendable type outright — it is not a strict-concurrency knob that can be dialled
# down per target. uniffi emits exactly one such global per `with_foreign` callback
# interface:
#
#   static let vtablePtr: UnsafePointer<UniffiVTableCallbackInterface…>
#
# which fails to compile ("not concurrency-safe because non-'Sendable' type … may have
# shared mutable state"). The pointer is written once during static initialization and
# only ever read afterwards, so `nonisolated(unsafe)` states the truth rather than
# suppressing a real race. Applied here because the glue is generated on every build and
# git-ignored, so it cannot be fixed by editing a checked-in file. Drop this step once
# uniffi emits the annotation itself.
echo "▸ Annotating uniffi callback vtables for Swift 6 concurrency"
for glue in "$GEN_DIR/capsule_core_ffi.swift" "$GEN_DIR/capsule_sdk.swift"; do
    perl -0pi -e 's/^(\s*)static let vtablePtr:/$1nonisolated(unsafe) static let vtablePtr:/mg' "$glue"
done

# uniffi emits `<namespace>FFI.modulemap` (one per namespace); an xcframework's headers
# directory must contain a file literally named `module.modulemap` — concatenating the
# per-namespace maps yields one file declaring every C module.
echo "▸ Preparing C headers + modulemap"
rm -rf "$HEADERS_DIR"
mkdir -p "$HEADERS_DIR"
cp "$GEN_DIR"/*FFI.h "$HEADERS_DIR/"
# uniffi's emitted modulemaps have no trailing newline, so a bare `cat` would glue
# `}module` together — append one after each part.
for mm in "$GEN_DIR"/*FFI.modulemap; do
    cat "$mm"
    echo
done >"$HEADERS_DIR/module.modulemap"

echo "▸ Lipo-ing the universal simulator library (arm64 + x86_64)"
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"
SIM_FAT_LIB="$BUILD_DIR/libcapsule_core_ffi_sim.a"
lipo -create \
    "$REPO_ROOT/target/$SIM_ARM_TARGET/release/$LIB_NAME" \
    "$REPO_ROOT/target/$SIM_X86_TARGET/release/$LIB_NAME" \
    -output "$SIM_FAT_LIB"

echo "▸ Assembling CapsuleCoreFFI.xcframework"
rm -rf "$XCFRAMEWORK"
xcodebuild -create-xcframework \
    -library "$REPO_ROOT/target/$DEVICE_TARGET/release/$LIB_NAME" -headers "$HEADERS_DIR" \
    -library "$SIM_FAT_LIB" -headers "$HEADERS_DIR" \
    -output "$XCFRAMEWORK" >/dev/null

echo "✓ FFI build complete"
echo "  xcframework : ${XCFRAMEWORK#"$REPO_ROOT/"}"
echo "  swift glue  : ${GEN_DIR#"$REPO_ROOT/"}/capsule_core_ffi.swift"
echo "  swift glue  : ${GEN_DIR#"$REPO_ROOT/"}/capsule_sdk.swift"
