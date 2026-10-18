#!/usr/bin/env bash
# Build libcapsule_sdk + generate the `capsule_sdk` uniffi Kotlin bindings and stage the generated
# source into this module (it is .gitignored — never commit generated code). Safe to re-run.
#
# For on-device (instrumented) StrongBox tests, also build the per-ABI JNI libs and copy them into
# src/main/jniLibs/<abi>/, e.g.:
#   (cd .. && mise run build-android)
#   cp ../target/aarch64-linux-android/debug/libcapsule_sdk.so src/main/jniLibs/arm64-v8a/
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"

cd "$repo"
mise run gen-bindings   # builds target/debug/libcapsule_sdk.{so,dylib} and emits target/bindings/kotlin

dest="$here/build/generated-bindings/uniffi/capsule_sdk"
mkdir -p "$dest"
cp target/bindings/kotlin/uniffi/capsule_sdk/capsule_sdk.kt "$dest/capsule_sdk.kt"
echo "staged kotlin bindings into capsule-core-kotlin/ (run: cd capsule-core-kotlin && ./gradlew test)"
