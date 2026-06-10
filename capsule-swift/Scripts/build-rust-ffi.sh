#!/usr/bin/env bash
#
# Build capsule-core-ffi for Apple platforms and package it as an xcframework
# alongside the generated Swift bindings. Outputs land in capsule-swift/.ffi/
# (git-ignored) and are consumed by the Tuist `CapsuleCatalog` module.
#
# The simulator slice is a universal binary (arm64 + x86_64) so the xcframework
# links on both Apple Silicon and Intel Macs.
#
# Run via `just build-ffi-apple`, or directly. Requires: rustup, cargo, xcodebuild.
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

# uniffi emits `<namespace>FFI.modulemap`; an xcframework's headers directory
# must contain a file literally named `module.modulemap`.
echo "▸ Preparing C headers + modulemap"
rm -rf "$HEADERS_DIR"
mkdir -p "$HEADERS_DIR"
cp "$GEN_DIR"/*FFI.h "$HEADERS_DIR/"
cat "$GEN_DIR"/*FFI.modulemap >"$HEADERS_DIR/module.modulemap"

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
