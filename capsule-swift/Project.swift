import ProjectDescription

// MARK: - Constants

private let bundlePrefix = "com.justin13888.capsule"
private let appDestinations: Destinations = [.iPhone, .iPad]
private let appDeploymentTargets: DeploymentTargets = .iOS("18.0")

/// Build settings shared by every Capsule target: Swift 6 language mode with
/// complete strict-concurrency checking.
private let capsuleSettings: Settings = .settings(base: [
    "SWIFT_VERSION": "6.0",
    "SWIFT_STRICT_CONCURRENCY": "complete",
])

// MARK: - Module factory

/// A framework module living under `Modules/<name>/Sources/**`, plus — when
/// `testDependencies` is non-`nil` — its unit-test target over
/// `Modules/<name>/Tests/**`.
///
/// Pass `testDependencies: []` for a test target that needs only the module
/// itself, or a list (e.g. `CapsuleTestSupport`) for the mocks it tests with.
private func module(
    _ name: String,
    sources: SourceFilesList = [],
    dependencies: [TargetDependency] = [],
    testDependencies: [TargetDependency]? = nil
) -> [Target] {
    let framework: Target = .target(
        name: name,
        destinations: appDestinations,
        product: .framework,
        bundleId: "\(bundlePrefix).\(name)",
        deploymentTargets: appDeploymentTargets,
        sources: sources.globs.isEmpty ? ["Modules/\(name)/Sources/**"] : sources,
        dependencies: dependencies,
        settings: capsuleSettings
    )
    guard let testDependencies else { return [framework] }
    let tests: Target = .target(
        name: "\(name)Tests",
        destinations: appDestinations,
        product: .unitTests,
        bundleId: "\(bundlePrefix).\(name)Tests",
        deploymentTargets: appDeploymentTargets,
        sources: ["Modules/\(name)/Tests/**"],
        dependencies: [.target(name: name)] + testDependencies,
        settings: capsuleSettings
    )
    return [framework, tests]
}

// MARK: - Targets

private let supportDependency: TargetDependency = .target(name: "CapsuleTestSupport")

private let moduleTargets: [Target] =
    // Foundation — value types, logging, utilities. No dependencies.
    module("CapsuleFoundation", testDependencies: [])

        // Catalog — Swift adapter over the Rust UniFFI catalog. Compiles the
        // generated bindings and links the prebuilt xcframework.
        + module(
            "CapsuleCatalog",
            sources: [
                "Modules/CapsuleCatalog/Sources/**",
                ".ffi/generated/capsule_core_ffi.swift",
            ],
            dependencies: [
                .target(name: "CapsuleFoundation"),
                .xcframework(path: ".ffi/CapsuleCoreFFI.xcframework"),
            ],
            testDependencies: [supportDependency]
        )

        // Managed store — Swift filesystem layer, hashing, import pipeline.
        + module(
            "ManagedStore",
            dependencies: [
                .target(name: "CapsuleFoundation"),
                .target(name: "CapsuleCatalog"),
            ],
            testDependencies: [supportDependency]
        )

        // Asset provider abstraction over PhotoKit + the managed store.
        + module(
            "AssetKit",
            dependencies: [
                .target(name: "CapsuleFoundation"),
                .target(name: "ManagedStore"),
            ],
            testDependencies: [supportDependency]
        )

        // Test-only mocks and fixtures, linked only by unit-test targets.
        + module(
            "CapsuleTestSupport",
            dependencies: [
                .target(name: "CapsuleFoundation"),
                .target(name: "CapsuleCatalog"),
                .target(name: "ManagedStore"),
                .target(name: "AssetKit"),
            ]
        )

        // Image decode / downsample / cache / prefetch pipeline.
        + module("ImagePipeline", dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "AssetKit"),
        ])

        // Design system + shared UI components (incl. the photo grid).
        + module("CapsuleUI", dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "ImagePipeline"),
            .target(name: "AssetKit"),
        ])

        // Feature modules.
        + module(
            "FeatureTimeline",
            dependencies: [
                .target(name: "CapsuleUI"),
                .target(name: "AssetKit"),
                .target(name: "ImagePipeline"),
            ],
            testDependencies: [supportDependency]
        )
        + module("FeatureViewer", dependencies: [
            .target(name: "CapsuleUI"),
            .target(name: "AssetKit"),
            .target(name: "ImagePipeline"),
        ])
        + module("FeatureAlbums", dependencies: [
            .target(name: "CapsuleUI"),
            .target(name: "AssetKit"),
        ])
        + module("FeatureSearch", dependencies: [
            .target(name: "CapsuleUI"),
            .target(name: "AssetKit"),
        ])

/// The thin iOS / iPadOS app target — composition root only.
private let appTarget: Target = .target(
    name: "Capsule",
    destinations: appDestinations,
    product: .app,
    bundleId: "\(bundlePrefix).Capsule",
    deploymentTargets: appDeploymentTargets,
    infoPlist: .extendingDefault(with: [
        "UILaunchScreen": ["UIColorName": ""],
        "NSPhotoLibraryUsageDescription":
            "Capsule shows and organizes the photos and videos in your library.",
    ]),
    sources: ["App/iOS/Sources/**"],
    dependencies: [
        .target(name: "FeatureTimeline"),
        .target(name: "FeatureViewer"),
        .target(name: "FeatureAlbums"),
        .target(name: "FeatureSearch"),
        .target(name: "CapsuleUI"),
        .target(name: "ImagePipeline"),
        .target(name: "AssetKit"),
        .target(name: "CapsuleFoundation"),
    ],
    settings: capsuleSettings
)

/// Every unit-test target — gathered for the `Capsule` scheme's test action.
private let testTargetNames: [TestableTarget] = [
    "CapsuleFoundationTests",
    "CapsuleCatalogTests",
    "ManagedStoreTests",
    "AssetKitTests",
    "FeatureTimelineTests",
]

// MARK: - Project

let project = Project(
    name: "Capsule",
    targets: moduleTargets + [appTarget],
    schemes: [
        .scheme(
            name: "Capsule",
            shared: true,
            buildAction: .buildAction(targets: ["Capsule"]),
            testAction: .targets(testTargetNames),
            runAction: .runAction(executable: "Capsule")
        ),
    ]
)
