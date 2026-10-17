import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - MockEnvironment

/// Every port, wired into one coherent world.
///
/// The app's composition root is one line:
///
/// ```swift
/// let environment = MockEnvironment(scenario: .resolve())
/// ```
///
/// and every screen reads its port off this. The value of building the whole
/// graph in one place is that a scenario cannot be half-applied: there is no
/// path where the timeline believes it is offline and the sync badge does not,
/// because both read stores built from the same ``MockConfiguration``.
///
/// It is a `struct` rather than an actor because it owns no mutable state — the
/// actors behind it do. Holding it is free, and passing it into a view hierarchy
/// costs nothing.
public struct MockEnvironment: Sendable {
    /// Which world this is.
    public let scenario: MockScenario
    /// The resolved configuration, exposed so a diagnostics screen can show what
    /// it is running against and a test can assert on it.
    public let configuration: MockConfiguration

    // MARK: Stores

    /// The library graph: assets, edits, container albums, smart albums.
    public let libraryStore: MockLibraryStore
    /// Model slots, people, places, search.
    public let intelligenceStore: MockIntelligenceStore
    /// Imports, uploads, sync, storage, quota.
    public let transferStore: MockTransferStore
    /// Sessions, devices, enrollment, recovery.
    public let identityStore: MockIdentityStore
    /// Quarantine, maintenance, local settings.
    public let systemStore: MockSystemStore
    /// Aggregated albums, peering, moderation.
    public let federationStore: MockFederationStore
    /// Share links and the drop inbox.
    public let sharingStore: MockSharingStore
    /// Procedural thumbnails. Not a port — the image pipeline reads it directly.
    public let thumbnails: MockThumbnailRenderer

    // MARK: Ports

    public var library: any LibraryPort { libraryStore }
    public var organize: any OrganizePort { libraryStore }
    public var stacks: any StackPort { libraryStore }
    public var albums: any AlbumPort { libraryStore }
    /// An adapter rather than the store itself: ``AlbumPort`` and
    /// ``SmartAlbumPort`` both declare `changes() -> AsyncStream<Void>`, and a
    /// container-album commit is not a predicate edit.
    public var smartAlbums: any SmartAlbumPort { MockSmartAlbumPortAdapter(store: libraryStore) }
    public var intelligence: any AIPort { intelligenceStore }
    public var people: any PeoplePort { intelligenceStore }
    public var places: any PlacesPort { intelligenceStore }
    public var search: any SearchPort { intelligenceStore }
    public var importing: any ImportPort { transferStore }
    public var uploads: any UploadPort { transferStore }
    public var sync: any SyncPort { transferStore }
    public var storage: any StoragePort { transferStore }
    public var quota: any QuotaPort { transferStore }
    public var auth: any AuthPort { identityStore }
    public var devices: any DevicePort { identityStore }
    public var enrollment: any EnrollmentPort { identityStore }
    public var recovery: any RecoveryPort { identityStore }
    public var quarantine: any QuarantinePort { systemStore }
    public var maintenance: any MaintenancePort { systemStore }
    public var settings: any SettingsPort { systemStore }
    public var federation: any FederationPort { federationStore }
    public var peering: any PeeringPort { federationStore }
    public var moderation: any ModerationPort { federationStore }
    public var sharing: any SharePort { sharingStore }
    public var drops: any DropPort { sharingStore }

    // MARK: Construction

    /// Build the whole graph for a scenario.
    ///
    /// - Parameters:
    ///   - scenario: which world. Defaults to whatever `-mock-scenario` names.
    ///   - clock: the injected clock. Nothing in this module reads `Date()`, so
    ///     passing a fixed instant makes every countdown, expiry, and staleness
    ///     check reproducible.
    ///   - seed: the world seed. Two environments with the same seed are
    ///     identical down to the last asset.
    public init(
        scenario: MockScenario = .healthy,
        clock: MockClock = .reference,
        seed: UInt64 = 0x0C0F_FEE0_1234_5678
    ) {
        self.init(configuration: MockConfiguration.make(scenario: scenario, clock: clock, seed: seed))
    }

    /// Build from an already-resolved configuration, for a test that wants to
    /// vary one knob without inventing a scenario for it.
    public init(configuration: MockConfiguration) {
        scenario = configuration.scenario
        self.configuration = configuration
        let library = MockLibraryStore(configuration: configuration)
        libraryStore = library
        intelligenceStore = MockIntelligenceStore(store: library, configuration: configuration)
        transferStore = MockTransferStore(store: library, configuration: configuration)
        identityStore = MockIdentityStore(configuration: configuration)
        systemStore = MockSystemStore(configuration: configuration)
        federationStore = MockFederationStore(store: library, configuration: configuration)
        sharingStore = MockSharingStore(store: library, configuration: configuration)
        thumbnails = MockThumbnailRenderer(library: library.library)
    }

    /// Build for the scenario named on the command line.
    ///
    /// The composition root and the UI tests agree through the raw strings in
    /// ``MockScenario``, because a UI-test bundle links the app as a target and
    /// cannot import this module.
    public static func fromLaunchArguments(
        _ processInfo: ProcessInfo = .processInfo,
        clock: MockClock = .reference
    ) -> MockEnvironment {
        MockEnvironment(scenario: .resolve(from: processInfo), clock: clock)
    }
}
