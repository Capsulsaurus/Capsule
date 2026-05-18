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

/// A framework module living under `Modules/<name>/Sources/**`.
private func module(
    _ name: String,
    sources: SourceFilesList = [],
    dependencies: [TargetDependency] = []
) -> Target {
    .target(
        name: name,
        destinations: appDestinations,
        product: .framework,
        bundleId: "\(bundlePrefix).\(name)",
        deploymentTargets: appDeploymentTargets,
        sources: sources.globs.isEmpty ? ["Modules/\(name)/Sources/**"] : sources,
        dependencies: dependencies,
        settings: capsuleSettings
    )
}

// MARK: - Project

let project = Project(
    name: "Capsule",
    targets: [
        // Foundation — value types, logging, utilities. No dependencies.
        module("CapsuleFoundation"),

        // Catalog — Swift adapter over the Rust UniFFI catalog. Compiles the
        // generated bindings and links the prebuilt xcframework.
        module(
            "CapsuleCatalog",
            sources: [
                "Modules/CapsuleCatalog/Sources/**",
                ".ffi/generated/capsule_core_ffi.swift",
            ],
            dependencies: [
                .target(name: "CapsuleFoundation"),
                .xcframework(path: ".ffi/CapsuleCoreFFI.xcframework"),
            ]
        ),

        // Managed store — Swift filesystem layer + import pipeline.
        module("ManagedStore", dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "CapsuleCatalog"),
        ]),

        // Asset provider abstraction over PhotoKit + the managed store.
        module("AssetKit", dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "ManagedStore"),
        ]),

        // Image decode / downsample / cache / prefetch pipeline.
        module("ImagePipeline", dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "AssetKit"),
        ]),

        // Design system + shared UI components (incl. the photo grid).
        module("CapsuleUI", dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "ImagePipeline"),
            .target(name: "AssetKit"),
        ]),

        // Feature modules.
        module("FeatureTimeline", dependencies: [
            .target(name: "CapsuleUI"),
            .target(name: "AssetKit"),
            .target(name: "ImagePipeline"),
        ]),
        module("FeatureViewer", dependencies: [
            .target(name: "CapsuleUI"),
            .target(name: "AssetKit"),
            .target(name: "ImagePipeline"),
        ]),
        module("FeatureAlbums", dependencies: [
            .target(name: "CapsuleUI"),
            .target(name: "AssetKit"),
        ]),
        module("FeatureSearch", dependencies: [
            .target(name: "CapsuleUI"),
            .target(name: "AssetKit"),
        ]),

        // Thin iOS/iPadOS app target — composition root only.
        .target(
            name: "Capsule",
            destinations: appDestinations,
            product: .app,
            bundleId: "\(bundlePrefix).Capsule",
            deploymentTargets: appDeploymentTargets,
            infoPlist: .extendingDefault(with: [
                "UILaunchScreen": ["UIColorName": ""],
            ]),
            sources: ["App/iOS/Sources/**"],
            dependencies: [
                .target(name: "FeatureTimeline"),
                .target(name: "FeatureViewer"),
                .target(name: "FeatureAlbums"),
                .target(name: "FeatureSearch"),
                .target(name: "CapsuleUI"),
                .target(name: "CapsuleFoundation"),
            ],
            settings: capsuleSettings
        ),
    ]
)
