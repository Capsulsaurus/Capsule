#!/usr/bin/env bash
# Build libcapsule_core + generate the uniffi Swift bindings and stage the generated sources into
# this package (they are .gitignored — never commit generated code). Safe to re-run.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"

cd "$repo"
just gen-bindings   # builds target/debug/libcapsule_core.dylib and emits target/bindings/swift

cp target/bindings/swift/capsule_core.swift "$here/Sources/CapsuleHardware/capsule_core.swift"
cp target/bindings/swift/capsule_coreFFI.h "$here/Sources/capsule_coreFFI/include/capsule_coreFFI.h"
echo "staged bindings into capsule-core-swift/ (run: cd capsule-core-swift && swift test)"
