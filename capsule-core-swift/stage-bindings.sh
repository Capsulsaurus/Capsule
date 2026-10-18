#!/usr/bin/env bash
# Build libcapsule_sdk + generate the `capsule_sdk` uniffi Swift bindings and stage the generated
# sources into this package (they are .gitignored — never commit generated code). Safe to re-run.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"

cd "$repo"
mise run gen-bindings   # builds target/debug/libcapsule_sdk.dylib and emits target/bindings/swift

cp target/bindings/swift/capsule_sdk.swift "$here/Sources/CapsuleHardware/capsule_sdk.swift"
cp target/bindings/swift/capsule_sdkFFI.h "$here/Sources/capsule_sdkFFI/include/capsule_sdkFFI.h"
echo "staged bindings into capsule-core-swift/ (run: cd capsule-core-swift && swift test)"
