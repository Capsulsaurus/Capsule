# Capsule monorepo task runner
# Plain name = auto-fix, -check suffix = verify-only

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

# ── Rust ─────────────────────────────────────────────────────────────────────

[group('rust')]
format-rust:
    cargo fmt

[group('rust')]
format-check-rust:
    cargo fmt --check

[group('rust')]
lint-rust:
    cargo clippy --workspace --exclude capsule-sdk --fix --allow-dirty

[group('rust')]
lint-check-rust:
    cargo clippy --workspace --exclude capsule-sdk -- -D warnings

[group('rust')]
test-rust:
    cargo test --workspace --exclude capsule-sdk

[group('rust')]
test-coverage-rust:
    cargo llvm-cov --workspace --exclude capsule-sdk --fail-under-lines 0

[group('rust')]
build-rust:
    cargo build --workspace --exclude capsule-sdk

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
    cd capsule-web && bun test

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
