# Capsule monorepo task runner
# Plain name = auto-fix, -check suffix = verify-only

# Curated clippy lint set, applied `--workspace` so every crate gets the identical rules
# (no per-crate `[lints]` opt-in/opt-out). Levels live here; thresholds in clippy.toml.
# Enables `pedantic` then allows the doc/cast/ergonomics noise; keeps the high-value checks
# (unwrap_used denied, mechanical cleanups) plus a couple of rustc lints.
clippy_flags := "-W clippy::pedantic -W unreachable_pub -D warnings -A clippy::must_use_candidate -A clippy::missing_errors_doc -A clippy::missing_panics_doc -A clippy::doc_markdown -A clippy::cast_possible_truncation -A clippy::cast_possible_wrap -A clippy::cast_sign_loss -A clippy::cast_lossless -A clippy::cast_precision_loss -A clippy::module_name_repetitions -A clippy::similar_names -A clippy::too_many_lines -A clippy::struct_excessive_bools -A clippy::unused_self -A clippy::return_self_not_must_use -A clippy::needless_pass_by_value -A clippy::trivially_copy_pass_by_ref -A clippy::unnecessary_wraps -A clippy::wildcard_imports -A clippy::default_trait_access -A clippy::upper_case_acronyms -A clippy::unused_async -A clippy::unused_async_trait_impl -A clippy::decimal_bitwise_operands -A clippy::wrong_self_convention -A clippy::enum_variant_names -A clippy::struct_field_names -A clippy::option_option -A clippy::used_underscore_binding -A clippy::ref_option -A clippy::items_after_statements -D clippy::unwrap_used -D clippy::dbg_macro"

# ── Aggregate: format ────────────────────────────────────────────────────────

[group('all')]
format: format-rust format-web format-docs format-kotlin format-vision format-swift

[group('all')]
format-check: format-check-rust format-check-web format-check-docs format-check-kotlin format-check-vision format-check-swift

# ── Aggregate: lint ──────────────────────────────────────────────────────────

[group('all')]
lint: lint-rust lint-web lint-docs lint-kotlin lint-vision lint-swift

[group('all')]
lint-check: lint-check-rust lint-check-web lint-check-docs lint-check-kotlin lint-check-vision lint-check-swift

# ── Aggregate: test ──────────────────────────────────────────────────────────

[group('all')]
test: test-rust test-web test-kotlin

[group('all')]
test-coverage: test-coverage-rust

# ── Aggregate: build ─────────────────────────────────────────────────────────

[group('all')]
build: build-rust build-web build-docs build-kotlin build-swift

# ── Aggregate: check (CI gate) ───────────────────────────────────────────────

[group('all')]
check: format-check lint-check test

# ── Per-toolchain check aggregates (CI entrypoints) ──────────────────────────
# Each maps 1:1 to a CI job so the workflow stays consistent with the justfile.

[group('rust')]
check-rust: format-check-rust lint-check-rust build-rust build-ffi lint-check-ffi gen-bindings verify-examples

[group('web')]
check-web: format-check-web lint-check-web test-web build-web

[group('docs')]
check-docs: format-check-docs lint-check-docs build-docs

[group('vision')]
check-vision: format-check-vision lint-check-vision

# ── Rust ─────────────────────────────────────────────────────────────────────

[group('rust')]
format-rust:
    cargo fmt

[group('rust')]
format-check-rust:
    cargo fmt --check

[group('rust')]
lint-rust:
    cargo clippy --workspace --exclude capsule-sdk --fix --allow-dirty -- {{ clippy_flags }}

[group('rust')]
lint-check-rust:
    cargo clippy --workspace --exclude capsule-sdk -- {{ clippy_flags }}

[group('rust')]
test-rust:
    cargo test --workspace --exclude capsule-sdk
    cargo test -p capsule-core --features ffi

[group('rust')]
test-coverage-rust:
    cargo llvm-cov --workspace --exclude capsule-sdk --fail-under-lines 0

[group('rust')]
build-rust:
    cargo build --workspace --exclude capsule-sdk

# ── FFI: uniffi bindings for Kotlin/Swift ────────────────────────────────────
# capsule-core exposes a minimal `FfiWorkspace` over uniffi behind the `ffi`
# feature; `gen-bindings` emits the Kotlin/Swift sources from the compiled cdylib
# (library mode). The `ffi-bindgen` feature adds uniffi's CLI for that generator.

[group('rust')]
build-ffi:
    cargo build -p capsule-core --features ffi

[group('rust')]
lint-check-ffi:
    cargo clippy -p capsule-core --features ffi -- {{ clippy_flags }}

[group('rust')]
gen-bindings:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -p capsule-core --features ffi
    ext="so"; [ "$(uname)" = "Darwin" ] && ext="dylib"
    lib="target/debug/libcapsule_core.${ext}"
    out="target/bindings"
    rm -rf "$out"
    for lang in kotlin swift; do
        cargo run -q -p capsule-core --features ffi-bindgen --bin uniffi-bindgen -- \
            generate --library "$lib" --language "$lang" --out-dir "$out/$lang"
    done
    # Smoke: both languages must have produced non-empty sources.
    test -s "$out/kotlin/uniffi/capsule_core/capsule_core.kt"
    test -s "$out/swift/capsule_core.swift"
    echo "uniffi bindings written to $out"

# ── HardwareSigner examples: existence + the Linux software smoke ─────────────
# Each device-key backend ships a HardwareSigner reference example. CI verifies they exist and
# runs the software-backend smoke (pure Rust — no uniffi/hardware/TPM). The native (Kotlin/Swift)
# and TPM examples are exercised locally per their READMEs; see capsule-core-{kotlin,swift}.
[group('rust')]
verify-examples:
    #!/usr/bin/env bash
    set -euo pipefail
    for f in \
        capsule-core/src/crypto/keys/software.rs \
        capsule-core/src/crypto/keys/tpm.rs \
        capsule-core-kotlin/src/main/kotlin/com/justin13888/capsule/hardware/SoftwareSigner.kt \
        capsule-core-kotlin/src/main/kotlin/com/justin13888/capsule/hardware/StrongBoxSigner.kt \
        capsule-core-swift/Sources/CapsuleHardware/SoftwareSigner.swift \
        capsule-core-swift/Sources/CapsuleHardware/SecureEnclaveSigner.swift; do
        test -f "$f" || { echo "missing HardwareSigner example: $f" >&2; exit 1; }
    done
    # The Linux software-signer smoke: the simplest backend (no uniffi foreign trait, no hardware).
    cargo test -p capsule-core --features ffi --lib crypto::keys::software
    echo "HardwareSigner examples present; software-signer smoke passed"

# ── Cross-compilation: FFI / mobile targets ──────────────────────────────────
# capsule-core builds as cdylib+staticlib (see its [lib] crate-type) so it links
# into iOS/Android. This is the formal definition of the targets the crate
# verifies; .github/workflows/ci.yml `rust-cross` enforces it.
#
#   Tier 1 — CI-gated on every PR:
#     x86_64-unknown-linux-gnu      (host; covered by the `rust` workspace build)
#     aarch64-apple-ios             aarch64-apple-ios-sim   x86_64-apple-ios
#     aarch64-apple-darwin          x86_64-apple-darwin
#     aarch64-linux-android         armv7-linux-androideabi
#     x86_64-linux-android          i686-linux-android
#   Tier 2 — best-effort (build-only, non-blocking):
#     x86_64-pc-windows-msvc        aarch64-unknown-linux-gnu
#
# Apple builds natively on macOS; Android via cargo-ndk; aarch64 Linux via `cross`
# (none can build Apple). build-apple skips off-macOS — a platform constraint, not a
# fixable dependency. build-android / build-linux-cross instead FAIL when their
# toolchain is missing, so a silent skip never masks an un-built target; install the
# toolchains (`just targets-add`, an Android NDK, `cross`) before `build-targets`.

[group('rust')]
targets-add:
    rustup target add \
        aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios \
        aarch64-apple-darwin x86_64-apple-darwin \
        aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android
    cargo install cargo-ndk

[group('rust')]
build-apple:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "$(uname)" != "Darwin" ]; then echo "Skipping Apple targets (not macOS)"; exit 0; fi
    for t in aarch64-apple-darwin x86_64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios; do
        echo ":: building capsule-core for $t"
        cargo build -p capsule-core --features ffi --target "$t"
    done

[group('rust')]
build-android:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v cargo-ndk >/dev/null 2>&1; then echo "build-android: cargo-ndk missing; run 'just targets-add'" >&2; exit 1; fi
    if [ -z "${ANDROID_NDK_HOME:-}${ANDROID_NDK_ROOT:-}" ]; then echo "build-android: set ANDROID_NDK_HOME (or ANDROID_NDK_ROOT) to your Android NDK" >&2; exit 1; fi
    cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -t x86 --platform 26 build -p capsule-core --features ffi

[group('rust')]
build-linux-cross:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v cross >/dev/null 2>&1; then echo "build-linux-cross: cross not installed; run 'cargo install cross'" >&2; exit 1; fi
    cross build -p capsule-core --features ffi --target aarch64-unknown-linux-gnu

[group('rust')]
build-windows:
    cargo build -p capsule-core --features ffi --target x86_64-pc-windows-msvc

[group('rust')]
build-targets: build-apple build-android build-linux-cross

# ── Web ──────────────────────────────────────────────────────────────────────

[group('web')]
format-web:
    cd capsule-web && bunx biome format --write .

[group('web')]
format-check-web:
    cd capsule-web && bunx biome format .

[group('web')]
lint-web:
    cd capsule-web && bunx biome check --write .

[group('web')]
lint-check-web:
    cd capsule-web && bunx biome check .

[group('web')]
test-web:
    cd capsule-web && bun test --pass-with-no-tests

[group('web')]
build-web:
    cd capsule-web && bun run build

# ── Docs ─────────────────────────────────────────────────────────────────────

[group('docs')]
format-docs:
    cd capsule-docs && bunx biome format --write .

[group('docs')]
format-check-docs:
    cd capsule-docs && bunx biome format .

[group('docs')]
lint-docs:
    cd capsule-docs && bunx biome check --write .

[group('docs')]
lint-check-docs:
    cd capsule-docs && bunx biome check .

[group('docs')]
build-docs:
    cd capsule-docs && bun run build

# ── Kotlin ───────────────────────────────────────────────────────────────────

[group('kotlin')]
format-kotlin:
    ./gradlew ktlintFormat

[group('kotlin')]
format-check-kotlin:
    ./gradlew ktlintCheck

[group('kotlin')]
lint-kotlin:
    ./gradlew detekt

[group('kotlin')]
lint-check-kotlin:
    ./gradlew detekt

[group('kotlin')]
test-kotlin:
    ./gradlew test

[group('kotlin')]
build-kotlin:
    ./gradlew build

# ── Swift ────────────────────────────────────────────────────────────────────

[group('swift')]
format-swift:
    #!/usr/bin/env bash
    if [ "$(uname)" != "Darwin" ]; then
        echo "Skipping swift format (not macOS)"
        exit 0
    fi
    cd capsule-swift && mise exec -- swiftformat .

[group('swift')]
format-check-swift:
    #!/usr/bin/env bash
    if [ "$(uname)" != "Darwin" ]; then
        echo "Skipping swift format check (not macOS)"
        exit 0
    fi
    cd capsule-swift && mise exec -- swiftformat --lint .

[group('swift')]
lint-swift:
    #!/usr/bin/env bash
    if [ "$(uname)" != "Darwin" ]; then
        echo "Skipping swiftlint (not macOS)"
        exit 0
    fi
    cd capsule-swift && mise exec -- swiftlint --fix --quiet && mise exec -- swiftlint

[group('swift')]
lint-check-swift:
    #!/usr/bin/env bash
    if [ "$(uname)" != "Darwin" ]; then
        echo "Skipping swiftlint check (not macOS)"
        exit 0
    fi
    cd capsule-swift && mise exec -- swiftlint --strict

# Cross-compile the Rust core and package CapsuleCoreFFI.xcframework.
[group('swift')]
build-ffi-apple:
    #!/usr/bin/env bash
    if [ "$(uname)" != "Darwin" ]; then
        echo "Skipping FFI build (not macOS)"
        exit 0
    fi
    bash capsule-swift/Scripts/build-rust-ffi.sh

# Generate the Xcode workspace with Tuist.
[group('swift')]
generate-swift:
    #!/usr/bin/env bash
    if [ "$(uname)" != "Darwin" ]; then
        echo "Skipping tuist generate (not macOS)"
        exit 0
    fi
    cd capsule-swift && mise exec -- tuist generate --no-open

# One-shot: build the FFI xcframework, then generate the workspace.
[group('swift')]
setup-swift: build-ffi-apple generate-swift

[group('swift')]
build-swift: build-ffi-apple
    #!/usr/bin/env bash
    if [ "$(uname)" != "Darwin" ]; then
        echo "Skipping swift build (not macOS)"
        exit 0
    fi
    set -o pipefail
    cd capsule-swift
    mise exec -- tuist generate --no-open
    xcodebuild -workspace Capsule.xcworkspace -scheme Capsule -configuration Debug \
        -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO build \
        | mise exec -- xcbeautify

# ── Vision ───────────────────────────────────────────────────────────────────

[group('vision')]
format-vision:
    cd capsule-vision && uv run ruff format

[group('vision')]
format-check-vision:
    cd capsule-vision && uv run ruff format --check

[group('vision')]
lint-vision:
    cd capsule-vision && uv run ruff check --fix

[group('vision')]
lint-check-vision:
    cd capsule-vision && uv run ruff check && uv run ty check

# ── Setup ────────────────────────────────────────────────────────────────────

[group('setup')]
hooks-install:
    lefthook install

[group('setup')]
hooks-uninstall:
    lefthook uninstall

[group('setup')]
install:
    cd capsule-web && bun install
    cd capsule-docs && bun install
    cd capsule-vision && uv sync
