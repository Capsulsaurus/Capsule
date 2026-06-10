// swift-tools-version:6.0
import Foundation
import PackageDescription

// Standalone harness that links the compiled `capsule-core` and proves the uniffi Swift bindings
// work before any app integration. Run `./stage-bindings.sh` first: it builds
// libcapsule_core.dylib and stages the generated `capsule_core.swift` + `capsule_coreFFI.h`.
//
// Absolute path to the repo's cargo target dir, so the linker and the runtime loader find the
// dylib regardless of the directory `swift test` runs from.
let targetDebug = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent() // capsule-core-swift/
    .deletingLastPathComponent() // repo root
    .appendingPathComponent("target/debug")
    .path

let package = Package(
    name: "CapsuleHardware",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "CapsuleHardware", targets: ["CapsuleHardware"]),
    ],
    targets: [
        // The C module exposing the generated uniffi FFI header (capsule_coreFFI.h).
        .target(name: "capsule_coreFFI"),
        // The generated Swift bindings + the per-platform HardwareSigner references.
        .target(
            name: "CapsuleHardware",
            dependencies: ["capsule_coreFFI"],
            linkerSettings: [
                .unsafeFlags([
                    "-L\(targetDebug)",
                    "-lcapsule_core",
                    "-Xlinker", "-rpath", "-Xlinker", targetDebug,
                ]),
            ]
        ),
        .testTarget(
            name: "CapsuleHardwareTests",
            dependencies: ["CapsuleHardware"]
        ),
    ],
    // uniffi's generated Swift is not Swift-6 strict-concurrency clean; compile in language
    // mode 5 (what uniffi targets). Our hand-written signers are concurrency-simple regardless.
    swiftLanguageModes: [.v5]
)
