import ProjectDescription

// MARK: - Constants

private let bundlePrefix = "com.justin13888.capsule"
/// Every target ships for all three Apple platforms. macOS is a native
/// destination, not Catalyst: the Mac build gets a real `NavigationSplitView`,
/// menu-bar commands, a `Settings` scene, and multiple windows.
private let appDestinations: Destinations = [.iPhone, .iPad, .mac]

/// The OS floor is 26 on both platforms. That is a deliberate product decision:
/// it makes the Liquid Glass design system, `.tabBarMinimizeBehavior`, and
/// `.navigationTransition(.zoom)` available unconditionally, so no UI code
/// carries an `#available` fence and there is exactly one visual language to
/// design and audit.
private let appDeploymentTargets: DeploymentTargets = .multiplatform(iOS: "26.0", macOS: "26.0")

/// The iOS-only floor, for targets that do not ship on the Mac. Tuist lints a
/// target whose deployment platforms exceed its destinations, so a target
/// restricted to iPhone and iPad must declare an iOS-only deployment target.
private let iOSOnlyDeploymentTargets: DeploymentTargets = .iOS("26.0")

/// Whether to build the Rust-backed half of the app.
///
/// **Off by default.** The app runs on `CapsuleMock` — an in-memory
/// implementation of every port — so `tuist generate && xcodebuild test`
/// works from a clean checkout with no Rust toolchain, no cross-compile, and
/// no `.ffi/` directory. That is what keeps the UI lane cheap to build and
/// verify, and it is the honest expression of "the UI is fully mocked".
///
/// Set `TUIST_FFI=1` to add `CapsuleCatalogFFI`, which compiles the generated
/// uniffi glue and links `CapsuleCoreFFI.xcframework`. Run
/// `mise run build-ffi-apple` first — the xcframework must already exist.
private let ffiEnabled = Environment.ffi.getBoolean(default: false)

/// `NSPhotoLibraryUsageDescription`, declared **only** in the FFI lane.
///
/// The mock lane's library is entirely synthetic and nothing in it constructs
/// `PhotoKitProvider` — so declaring photo-library usage there is a permission
/// the app asks for and never exercises. That is not cosmetic. iOS presents the
/// limited-library alert automatically at launch for any app that declares the
/// key and holds a `.limited` grant, which is why a fully mocked build was
/// greeting the user with a Photos prompt over the home screen before it had
/// drawn a single pixel — with no call anywhere in our own code. It also
/// contradicts the offline-first contract in *Local Gallery* FR1/FR2/NFR1: the
/// gallery must work with no system access at all, and an app that asks for
/// access it cannot use is not demonstrating that.
///
/// Omitting the key also makes the invariant self-enforcing. If a mock-lane code
/// path ever does reach PhotoKit, iOS terminates the process with a usage-
/// description crash instead of silently prompting — a loud failure naming the
/// exact call site, which is what we want.
///
/// The picker-driven import path needs no entry here either way: `PHPicker` and
/// SwiftUI's `PhotosPicker` run out of process and require no authorization. The
/// key belongs to *direct* PhotoKit access, which only the FFI lane will have.
///
/// Localized by `Generated/InfoPlist.xcstrings` from `app.infoplist.photo_library_usage`,
/// like the Face ID description declared on the app target; the literal here is the base
/// declaration and the en fallback, never the source of truth.
private let photoLibraryUsage: [String: Plist.Value] = ffiEnabled
    ? ["NSPhotoLibraryUsageDescription": "Capsule shows and organizes the photos and videos in your library."]
    : [:]

/// The Swift-6 language settings shared by every Capsule target. MARKETING_VERSION is
/// the iOS app's version source of truth, kept in sync across every package by
/// `mise run set-version` (xtask).
private let baseSettings: SettingsDictionary = [
    "SWIFT_VERSION": "6.0",
    "SWIFT_STRICT_CONCURRENCY": "complete",
    "MARKETING_VERSION": "0.1.0",
    "CURRENT_PROJECT_VERSION": "1",
]

/// Framework settings: a Release build marks the framework mergeable so the
/// app can fold it into its binary, cutting dylib loads at launch.
private let frameworkSettings: Settings = .settings(
    base: baseSettings,
    configurations: [
        .debug(name: "Debug"),
        .release(name: "Release", settings: ["MERGEABLE_LIBRARY": "YES"]),
    ]
)

/// App settings: a Release build merges its mergeable framework dependencies.
private let appSettings: Settings = .settings(
    base: baseSettings,
    configurations: [
        .debug(name: "Debug"),
        .release(name: "Release", settings: ["MERGED_BINARY_TYPE": "automatic"]),
    ]
)

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
        settings: frameworkSettings
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
        settings: frameworkSettings
    )
    return [framework, tests]
}

// MARK: - Targets

private let supportDependency: TargetDependency = .target(name: "CapsuleTestSupport")

private let moduleTargets: [Target] =
    // Foundation — value types, logging, utilities. No dependencies.
    module("CapsuleFoundation", testDependencies: [])

    // Domain — the display/domain value types, shaped as structural mirrors of
    // the uniffi records they will eventually be generated from. No I/O, no
    // platform, no strings: this is the vocabulary every other module speaks.
    + module(
        "CapsuleDomain",
        dependencies: [.target(name: "CapsuleFoundation")],
        testDependencies: []
    )

    // Ports — the async protocol seams, one per capability. Feature modules
    // import ONLY this for data, so swapping the mock adapters for the real
    // uniffi ones is a change in the composition root and nowhere else.
    + module(
        "CapsulePorts",
        dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "CapsuleDomain"),
        ],
        testDependencies: []
    )

    // Navigation — one `Route` vocabulary, a router with a stack per section,
    // deep links, and the menu-command table. No SwiftUI views: the three shells
    // bind this differently, so keeping it view-free is what lets them share it.
    + module(
        "CapsuleNavigation",
        dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "CapsuleDomain"),
        ],
        testDependencies: []
    )

    // Mock — the in-memory implementation of every port, and the app's only
    // data source while the Rust core is being rebuilt. Ships (rather than
    // living in a test target) because the app itself runs on it.
    + module(
        "CapsuleMock",
        dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "CapsuleDomain"),
            .target(name: "CapsulePorts"),
        ],
        testDependencies: []
    )

    // Diagnostics — MetricKit crash/perf collection, consent, breadcrumbs,
    // redacted bug-report bundles, and an opt-in self-hosted uploader.
    + module(
        "CapsuleDiagnostics",
        dependencies: [.target(name: "CapsuleFoundation")],
        testDependencies: [supportDependency]
    )

    // Catalog — the catalog contract, its Swift-native models, and the
    // in-memory reference implementation. No Rust core: this half compiles
    // and tests everywhere, which is what makes the mock lane possible.
    + module(
        "CapsuleCatalog",
        dependencies: [.target(name: "CapsuleFoundation")],
        testDependencies: [supportDependency]
    )

    // Catalog (FFI) — the Rust-backed half: the generated uniffi glue for
    // both namespaces (the `capsule_core_ffi` catalog and `capsule-sdk`'s
    // `capsule_sdk` user flows, S-F3/S-D9), the record conversions, and the
    // error mapping onto the FFI-free `CatalogError`. Present only when
    // `TUIST_FFI=1`; nothing above it names a generated type.
    + (ffiEnabled
        ? module(
            "CapsuleCatalogFFI",
            sources: [
                "Modules/CapsuleCatalogFFI/Sources/**",
                ".ffi/generated/capsule_core_ffi.swift",
                ".ffi/generated/capsule_sdk.swift",
            ],
            dependencies: [
                .target(name: "CapsuleFoundation"),
                .target(name: "CapsuleCatalog"),
                .xcframework(path: ".ffi/CapsuleCoreFFI.xcframework"),
            ],
            testDependencies: []
        )
        : [])

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
            // The MockBridge adapters present the port-based world through the
            // older provider protocols the existing screens consume.
            .target(name: "CapsuleDomain"),
            .target(name: "CapsulePorts"),
        ],
        testDependencies: [supportDependency]
    )

    // Test-only mocks and fixtures, linked only by unit-test targets.
    + module(
        "CapsuleTestSupport",
        dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "CapsuleDiagnostics"),
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

    // Design system + shared UI components: the virtualized timeline geometry,
    // the shared photo grid, and `PlatformCollection/` — the one UIKit/AppKit
    // island every grid in the app is built on.
    //
    // It depends on `CapsuleDomain` but deliberately **not** on `CapsulePorts`:
    // the design system renders domain states — a seal, a cull flag, a sync
    // tier — but must never be able to fetch one. `AssetWindowStore` is generic
    // over a fetch closure for exactly that reason.
    + module(
        "CapsuleUI",
        dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "CapsuleDomain"),
            .target(name: "ImagePipeline"),
            .target(name: "AssetKit"),
        ],
        testDependencies: []
    )

    // Feature modules.
    + module(
        "FeatureTimeline",
        dependencies: [
            .target(name: "CapsuleUI"),
            .target(name: "AssetKit"),
            .target(name: "ImagePipeline"),
            .target(name: "FeatureViewer"),
        ],
        testDependencies: [supportDependency]
    )
    + module(
        "FeatureViewer",
        dependencies: [
            .target(name: "CapsuleUI"),
            .target(name: "AssetKit"),
            .target(name: "ImagePipeline"),
        ],
        testDependencies: [supportDependency]
    )
    + module(
        "FeatureAlbums",
        dependencies: [
            .target(name: "CapsuleUI"),
            .target(name: "AssetKit"),
            .target(name: "ImagePipeline"),
            .target(name: "FeatureViewer"),
            // Named because `AlbumsRootView` links a `Route`, which is the only
            // vocabulary the shell's navigation stack accepts. A feature module
            // takes this dependency when it names a route and not before.
            .target(name: "CapsuleNavigation"),
        ],
        testDependencies: [supportDependency]
    )
    + module(
        "FeatureSearch",
        dependencies: [
            .target(name: "CapsuleUI"),
            .target(name: "AssetKit"),
            .target(name: "ImagePipeline"),
            .target(name: "FeatureViewer"),
        ],
        testDependencies: [supportDependency]
    )

    // Transfers, quota, storage reclamation, sync status, and the
    // per-asset custody receipt.
    + module(
        "FeatureTransfer",
        dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "CapsuleDomain"),
            .target(name: "CapsulePorts"),
            .target(name: "CapsuleUI"),
            // SwiftUI previews are driven by real scenarios, so the mock is a
            // build dependency of the screens, not only of their tests.
            .target(name: "CapsuleMock"),
        ],
        testDependencies: []
    )

    // Onboarding, sign-in, the first-device enrollment ceremony,
    // recovery, and the device/session ledger.
    + module(
        "FeatureAuth",
        dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "CapsuleDomain"),
            .target(name: "CapsulePorts"),
            .target(name: "CapsuleNavigation"),
            .target(name: "CapsuleUI"),
            // SwiftUI previews are driven by real scenarios, so the mock is a
            // build dependency of the screens, not only of their tests.
            .target(name: "CapsuleMock"),
        ],
        testDependencies: []
    )

    // Share links, guest-drop inbox, LAN peering, federation, and
    // moderation.
    + module(
        "FeatureSharing",
        dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "CapsuleDomain"),
            .target(name: "CapsulePorts"),
            .target(name: "CapsuleNavigation"),
            .target(name: "CapsuleUI"),
            // SwiftUI previews are driven by real scenarios, so the mock is a
            // build dependency of the screens, not only of their tests.
            .target(name: "CapsuleMock"),
        ],
        testDependencies: []
    )

    // The eighteen-section settings tree — a grouped list on iOS,
    // a tabbed Settings window on macOS.
    + module(
        "FeatureSettings",
        dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "CapsuleDomain"),
            .target(name: "CapsulePorts"),
            .target(name: "CapsuleNavigation"),
            .target(name: "CapsuleUI"),
            // SwiftUI previews are driven by real scenarios, so the mock is a
            // build dependency of the screens, not only of their tests.
            .target(name: "CapsuleMock"),
        ],
        testDependencies: []
    )

    // The photo-import pipeline: source picker, scan, plan confirmation,
    // execution, and history.
    + module(
        "FeatureImport",
        dependencies: [
            .target(name: "CapsuleFoundation"),
            .target(name: "CapsuleDomain"),
            .target(name: "CapsulePorts"),
            .target(name: "CapsuleUI"),
            // SwiftUI previews are driven by real scenarios, so the mock is a
            // build dependency of the screens, not only of their tests.
            .target(name: "CapsuleMock"),
        ],
        testDependencies: []
    )

    // Collections home — albums, media types, places, utilities.
    + module(
        "FeatureCollections",
        dependencies: [
            .target(name: "CapsuleUI"),
            // `HiddenView` sits behind the SR1 local-auth gate, whose seam is a
            // port so the mocked app can satisfy it without the system sheet.
            .target(name: "CapsulePorts"),
            .target(name: "AssetKit"),
            .target(name: "ImagePipeline"),
            .target(name: "FeatureViewer"),
            .target(name: "FeatureAlbums"),
        ]
    )

/// The thin app target — composition root only, shared by all three platforms.
private let appTarget: Target = .target(
    name: "Capsule",
    destinations: appDestinations,
    product: .app,
    bundleId: "\(bundlePrefix).Capsule",
    deploymentTargets: appDeploymentTargets,
    infoPlist: .extendingDefault(with: [
        // iOS-only keys; macOS ignores them rather than erroring, so one plist
        // serves every destination.
        "UILaunchScreen": ["UIColorName": ""],
        // macOS: without a category the Mac app shows as "Developer Tools" in
        // Finder and the App Store. Photography is what this is.
        "LSApplicationCategoryType": "public.app-category.photography",
        // The `capsule://` scheme. Without this the OS has no idea the app
        // claims those URLs, so `onOpenURL` — and the whole parser, its
        // secret-redaction rules and its tests — could never be reached from
        // outside the process. Declared on every destination: `openURL` routes
        // by scheme on macOS exactly as it does on iOS.
        //
        // `https://<server>/s/<id>` share links are deliberately *not* here.
        // Universal links need an `associated-domains` entitlement, a signed
        // build, and an `apple-app-site-association` file served by each user's
        // own server — and Capsule is self-hosted, so there is no single domain
        // to claim. Reaching those needs a decision about how a self-hosted
        // server advertises its app, which is a design question rather than a
        // plist key.
        "CFBundleURLTypes": [
            [
                "CFBundleURLName": "\(bundlePrefix).Capsule",
                "CFBundleTypeRole": "Editor",
                "CFBundleURLSchemes": ["capsule"],
            ],
        ],
        // Usage descriptions are localized by `Generated/InfoPlist.xcstrings` (compiled
        // from `locales/` by `xtask i18n`, which owns the catalog key -> Info.plist key
        // mapping). The system still requires the key to be *declared* here — it refuses
        // the API outright when it is absent — so this value is the base declaration and
        // the en fallback, never the source of truth. Change the wording in `locales/`,
        // then mirror it here.
        //
        // Face ID is declared in **both** lanes, unlike the photo-library key below.
        // Declaring it prompts for nothing: it is the line the system shows inside a
        // challenge the app itself initiates, and a build that reaches `evaluatePolicy`
        // without it is terminated rather than degraded. `SystemLocalAuthenticator` is
        // reachable from the Security screen in either lane, so the key belongs in both.
        "NSFaceIDUsageDescription": // locales/: app.infoplist.face_id_usage
            "Capsule uses Face ID to unlock your Hidden and Recently Deleted photos.",
        // Let the simulator reach a dev server on http://127.0.0.1:3000 (`mise run serve-api`,
        // slice S-P7). `NSAllowsLocalNetworking` is scoped to loopback and .local — it does
        // NOT weaken ATS for real servers, unlike NSAllowsArbitraryLoads. Production
        // deployments are HTTPS and unaffected.
        "NSAppTransportSecurity": ["NSAllowsLocalNetworking": true],
    ].merging(photoLibraryUsage) { current, _ in current }),
    sources: ["App/Sources/**"],
    // The i18n string catalogs are generated by `xtask i18n` from `locales/`.
    // SwiftUI `LocalizedStringKey` and `String(localized:)` both resolve against
    // the main app bundle by default, so shipping the catalog in the app target
    // covers keys used by the feature-framework modules too. `InfoPlist.xcstrings`
    // localizes the usage descriptions declared in `infoPlist` above.
    resources: [
        "App/Resources/**",
        "Generated/Localizable.xcstrings",
        "Generated/InfoPlist.xcstrings",
    ],
    dependencies: [
        .target(name: "FeatureTimeline"),
        .target(name: "FeatureViewer"),
        .target(name: "FeatureCollections"),
        .target(name: "FeatureSearch"),
        .target(name: "FeatureAlbums"),
        // The composition root reaches every feature module by definition: it is
        // the one place a `Route` is turned into the screen that presents it, and
        // a screen it cannot name is a destination it cannot wire.
        .target(name: "FeatureAuth"),
        .target(name: "FeatureImport"),
        .target(name: "FeatureSettings"),
        .target(name: "FeatureSharing"),
        .target(name: "FeatureTransfer"),
        .target(name: "CapsuleUI"),
        .target(name: "ImagePipeline"),
        .target(name: "AssetKit"),
        .target(name: "CapsuleDiagnostics"),
        .target(name: "CapsuleFoundation"),
        .target(name: "CapsuleCatalog"),
        .target(name: "CapsuleDomain"),
        .target(name: "CapsulePorts"),
        .target(name: "CapsuleMock"),
        .target(name: "CapsuleNavigation"),
    ] + (ffiEnabled ? [.target(name: "CapsuleCatalogFFI")] : []),
    settings: appSettings
)

/// The composition root's own unit tests.
///
/// Hosted by the app because what they assert lives there: which `Route`
/// resolves to a real screen and which is still a scaffold is a fact about the
/// wiring, not about any module, and it is exactly the fact that rots silently.
/// swift-testing, like every other unit target here — XCTest stays confined to
/// the XCUITest bundle below.
private let appTestTarget: Target = .target(
    name: "CapsuleAppTests",
    destinations: appDestinations,
    product: .unitTests,
    bundleId: "\(bundlePrefix).CapsuleAppTests",
    deploymentTargets: appDeploymentTargets,
    sources: ["App/Tests/**"],
    dependencies: [.target(name: "Capsule")],
    settings: frameworkSettings
)

/// The XCUITest bundle — UI automation and the accessibility audits.
///
/// iPhone and iPad only. XCUITest on macOS needs a signed app and an automation
/// permission grant, neither of which a `CODE_SIGNING_ALLOWED=NO` CI build has;
/// the Mac gets the same coverage from unit tests plus the shared view models.
/// This is the one target where `import XCTest` is sanctioned.
private let uiTestTarget: Target = .target(
    name: "CapsuleAppUITests",
    destinations: [.iPhone, .iPad],
    product: .uiTests,
    bundleId: "\(bundlePrefix).CapsuleAppUITests",
    deploymentTargets: iOSOnlyDeploymentTargets,
    sources: ["UITests/Sources/**"],
    dependencies: [.target(name: "Capsule")],
    settings: frameworkSettings
)

/// Every unit-test target — gathered for the `Capsule` scheme's test action.
private let testTargetNames: [TestableTarget] = (ffiEnabled ? ["CapsuleCatalogFFITests"] : []) + [
    "CapsuleFoundationTests",
    "CapsuleDiagnosticsTests",
    "CapsuleCatalogTests",
    "CapsuleDomainTests",
    "CapsulePortsTests",
    "CapsuleNavigationTests",
    "CapsuleMockTests",
    "CapsuleUITests",
    "ManagedStoreTests",
    "AssetKitTests",
    "FeatureTimelineTests",
    "FeatureViewerTests",
    "FeatureAlbumsTests",
    "FeatureSearchTests",
    "FeatureTransferTests",
    "FeatureSharingTests",
    "FeatureAuthTests",
    "FeatureSettingsTests",
    "FeatureImportTests",
    "CapsuleAppTests",
]

// MARK: - Project

let project = Project(
    name: "Capsule",
    targets: moduleTargets + [appTarget, appTestTarget, uiTestTarget],
    schemes: [
        .scheme(
            name: "Capsule",
            shared: true,
            buildAction: .buildAction(targets: ["Capsule"]),
            testAction: .targets(testTargetNames),
            runAction: .runAction(executable: "Capsule")
        ),
        // UI automation is a separate scheme on purpose: it boots a simulator and
        // walks the app, which is far slower than the unit suites. `check-swift`
        // runs the fast scheme on every destination; this one runs on its own.
        .scheme(
            name: "CapsuleAppUITests",
            shared: true,
            buildAction: .buildAction(targets: ["Capsule", "CapsuleAppUITests"]),
            testAction: .targets(["CapsuleAppUITests"]),
            runAction: .runAction(executable: "Capsule")
        ),
    ]
)
