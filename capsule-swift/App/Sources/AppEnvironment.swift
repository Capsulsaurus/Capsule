import AssetKit
import CapsuleAlerts
import CapsuleCatalog
import CapsuleDiagnostics
import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsulePorts
import FeatureAuth
import FeatureSettings
import FeatureSharing
import FeatureTimeline
import FeatureViewer
import Foundation
import ImagePipeline
import ManagedStore

/// The app's composition root — it builds one world and hands every screen its
/// seam into it.
///
/// ## The mock lane composes no PhotoKit
///
/// Nothing in this file names a system-Photos type, and that is the whole
/// point. `local-gallery.md` FR1 says a native Capsule app is "a complete local
/// gallery first and a synced client second", and FR2 makes never-signed-in a
/// valid mode: install it, never connect a server, and the library still works.
/// A launch that opens on a system permission prompt fails both — it asks the
/// user to authorise access to somebody else's library before showing them
/// their own.
///
/// So the timeline is served by ``PortBackedAssetProvider`` over
/// ``MockEnvironment``'s ``LibraryPort``, whose ``AssetProvider/authorizationStatus()``
/// is `.authorized` without asking anyone, and whose reads perform **zero**
/// network I/O — NFR1's "not 'tolerate failure', but do not attempt".
/// `PhotoKitProvider` and its siblings still live in `AssetKit` for the FFI
/// lane, and are simply not referenced from here.
///
/// ## What the picker imports, the timeline shows
///
/// The picker-driven importer writes the on-disk managed store, so the
/// timeline is not the port-backed provider alone: it is a
/// ``CompositeAssetProvider`` over that provider and the store's own
/// ``ManagedProvider``. The importer is handed **the same** `ManagedProvider`
/// instance, so the refresh it publishes after an import is a change on a
/// source the timeline is subscribed to. Two instances would be two catalogs'
/// worth of truth and one of them unseen.
///
/// The timeline is not the only surface an import has to reach. Thumbnails and
/// viewer pixels, Recently Deleted, captions and the info panel's metadata are
/// each composed over both stores the same way, managed store first wherever
/// order decides ownership — see ``composeThumbnails(library:managed:)``,
/// ``composeTrash(library:managed:)``, ``composeCaptions(library:managed:)``
/// and ``composeMetadata(locations:managed:)``.
///
/// ## Two surfaces, on purpose
///
/// The existing screens consume the older `AssetKit` protocols and are not
/// being rewritten, so the `MockBridge` adapters project the ports onto them.
/// New screens should skip the projection and take the raw port they need —
/// both are exposed below, and the ports are the direction of travel.
@MainActor
struct AppEnvironment {
    // MARK: The world

    /// Every port, wired into one coherent scenario. Read the scenario name
    /// from `-mock-scenario`, so `xcrun simctl launch … -mock-scenario
    /// quarantine` and an Xcode scheme argument select the same world; with no
    /// such argument, from the override the Advanced settings screen recorded.
    let mock: MockEnvironment

    /// Which world this launch is running. Shown in diagnostics.
    var scenario: MockScenario { mock.scenario }

    // MARK: Bridged provider surface

    let assetProvider: any AssetProvider
    let albumProvider: any AlbumProvider
    let trashProvider: any TrashProvider
    let hiddenStore: HiddenStore
    let thumbnails: any ThumbnailProvider
    let mediaLoader: ViewerMediaLoader
    /// Reads and writes asset captions for the viewer's info panel.
    let captionStore: any CaptionStore
    /// Turns a photo's coordinates into a place name for the viewer's info panel.
    ///
    /// Composed here rather than defaulted at the view, because the default is
    /// `NoPlaceNameResolver` — deliberately, so a viewer built without an explicit
    /// resolver cannot make a network call by accident. Somewhere has to make the
    /// opposite choice explicitly, and a composition root is the only honest place
    /// for it. The per-device opt-in is checked *inside* the resolver, so this stays
    /// off until the user turns it on in Security & Privacy.
    let placeNames: any PlaceNameResolver
    let importer: LibraryImporter

    /// The local-authentication gate in front of Trash and Hidden (*SR1*).
    ///
    /// Stored rather than forwarded because it is a *system* seam rather than a
    /// library read, and the mock lane deliberately does not compose the system
    /// one: `LAContext` on a device with no enrolled credential presents a
    /// SpringBoard passcode sheet over the app, which is exactly the kind of
    /// out-of-process prompt this composition root exists to keep out.
    let localAuthenticator: any LocalAuthenticator

    /// Persisted diagnostics & telemetry consent (local-only by default).
    let consentStore: ConsentStore
    /// Wires MetricKit, breadcrumbs, the crash prompt, and bug-report export.
    let diagnostics: DiagnosticsCoordinator

    /// The local-alert loop: per-class arm / re-arm / cancel, permission at
    /// first use, and the bounded snooze. See ``makeAlerts(mock:)``.
    let alerts: AlertCoordinator
    /// The notification-center delegate, held for the app's lifetime so a tap
    /// — including one that launched the app — reaches the router.
    ///
    /// Not installed in the mock lane: nothing there is armed with the OS, so
    /// there is nothing to tap, and touching `UNUserNotificationCenter` at all
    /// is what the mock lane avoids. The lane that composes
    /// `UserNotificationAlertScheduler` calls `install()` beside it, before
    /// launch finishes.
    let alertResponder: AlertNotificationResponder

    // MARK: Raw ports

    // Forwarded rather than stored: `MockEnvironment` already owns the graph,
    // and re-declaring twenty-six stored properties would create a second place
    // for the wiring to be wrong.

    var library: any LibraryPort { mock.library }
    var organize: any OrganizePort { mock.organize }
    var stacks: any StackPort { mock.stacks }
    var albums: any AlbumPort { mock.albums }
    var smartAlbums: any SmartAlbumPort { mock.smartAlbums }

    var intelligence: any AIPort { mock.intelligence }
    var people: any PeoplePort { mock.people }
    var places: any PlacesPort { mock.places }
    var search: any SearchPort { mock.search }

    /// Labels on regions, stacks and albums, and the regions themselves.
    ///
    /// Stored rather than forwarded, like ``transferEvents``: it is one of the
    /// ``LanePorts`` a lane supplies, so the FFI lane's Rust-backed adapter
    /// replaces the mock one here and nowhere else.
    let labels: any LabelPort

    var importing: any ImportPort { mock.importing }
    var uploads: any UploadPort { mock.uploads }
    /// The step-by-step transfer event stream the real-time transfer view
    /// draws.
    let transferEvents: any TransferEventPort
    var sync: any SyncPort { mock.sync }
    var storage: any StoragePort { mock.storage }
    var quota: any QuotaPort { mock.quota }

    var auth: any AuthPort { mock.auth }
    var devices: any DevicePort { mock.devices }
    var enrollment: any EnrollmentPort { mock.enrollment }
    /// Wrapped so every verify, snooze or rotation refreshes the alerts: the
    /// port has no change stream for ``runAlertLoop()`` to follow.
    var recovery: any RecoveryPort {
        let alerts = alerts
        return AlertRefreshingRecoveryPort(mock.recovery) { _ = await alerts.refresh() }
    }

    var quarantine: any QuarantinePort { mock.quarantine }
    var maintenance: any MaintenancePort { mock.maintenance }
    var settings: any SettingsPort { mock.settings }

    // MARK: Ceremony ports

    // Sign-in, server discovery, and first-device enrollment are declared by
    // `FeatureAuth` rather than by `CapsulePorts`: they are ceremonies driven by
    // the SDK, not library reads, and the protocol that describes one belongs
    // beside the screens that run it. They are stored rather than forwarded
    // because `MockEnvironment` does not own them — this is the seam where a
    // real SDK-backed adapter will be substituted, and it is the only place that
    // has to change when it is.

    /// Domain lookup and server pinning, for the Connect-a-Server step.
    let serverDiscovery: any ServerDiscoveryPort
    /// Handle/password sign-in and account creation.
    let credentials: any LocalCredentialPort
    /// The staged first-device enrollment ceremony.
    let firstDeviceEnrollment: any FirstDeviceEnrollmentPort

    /// The moderation audit ledger and the appeals filed against it. Declared by
    /// `FeatureSharing` rather than `CapsulePorts` because an appeal is
    /// authenticated by master-key proof rather than by the session, which makes
    /// it a ceremony rather than a library read.
    let moderationRecords: any ModerationRecordPort
    /// Which unrecognised origins are being withheld pending a decision. Backed
    /// by the same store as ``moderationRecords`` — one ledger, two questions.
    let untrustedOriginPolicy: any UntrustedOriginPolicy

    var federation: any FederationPort { mock.federation }
    var peering: any PeeringPort { mock.peering }
    var moderation: any ModerationPort { mock.moderation }
    var sharing: any SharePort { mock.sharing }
    var drops: any DropPort { mock.drops }

    // MARK: Construction

    /// The world this launch runs: see ``launchWorld(_:defaults:)``.
    init() {
        self.init(mock: Self.launchWorld())
    }

    /// Read the scenario at launch.
    ///
    /// The command line wins: a launch that passes `-mock-scenario` — every UI
    /// test, every scheme that sets one — gets that world whatever the device
    /// has persisted. Only a launch that names none falls back to the
    /// ``MockScenarioSelection`` the Advanced screen wrote, and then to the
    /// healthy world. A scenario cannot be swapped under a running graph, which
    /// is why the screen records a choice and this is where it takes effect.
    static func launchWorld(
        _ processInfo: ProcessInfo = .processInfo,
        defaults: UserDefaults = .standard
    ) -> MockEnvironment {
        let persisted = MockScenarioSelection(defaults: defaults).selected()
        CapsuleLog.app.debug(
            "persisted mock scenario: \(persisted?.rawValue ?? "none", privacy: .public)"
        )
        return .fromLaunchArguments(processInfo, persisted: persisted)
    }

    /// The world over `mock`, with `lane`'s ports in place of the mock world's
    /// own where it supplies them.
    init(mock: MockEnvironment, lane: LanePorts? = nil) {
        self.mock = mock
        CapsuleLog.app.info("mock scenario: \(mock.scenario.rawValue, privacy: .public)")

        let lane = lane ?? .mock(mock)
        labels = lane.labels
        transferEvents = lane.transferEvents

        // Held concretely for a moment: the info panel's metadata source wants
        // this provider's `AssetLocationSource` conformance, which the erased
        // `any AssetProvider` the rest of the app sees does not carry.
        let library = PortBackedAssetProvider(library: mock.library, organize: mock.organize)
        let managed = Self.makeManagedLane(root: Self.libraryRoot(), openLibrary: lane.openManagedLibrary)
        assetProvider = Self.composeTimeline(library: library, managed: managed.provider)
        captionStore = Self.composeCaptions(
            library: PortBackedCaptionStore(library: mock.library, organize: mock.organize),
            managed: managed
        )
        albumProvider = PortBackedAlbumProvider(albums: mock.albums, library: mock.library)
        // Constructed here rather than at its own assignment below because the
        // trash adapter holds the SR1 gate and needs the same authenticator the
        // Security screen reports on — two instances would be two gates.
        let authenticator = MockLocalAuthenticator()
        localAuthenticator = authenticator
        trashProvider = Self.composeTrash(
            library: PortBackedTrashProvider(
                organize: mock.organize,
                library: mock.library,
                authenticator: authenticator
            ),
            managed: managed
        )
        hiddenStore = HiddenStore()
        let renderer = Self.composeThumbnails(
            library: PortBackedThumbnailProvider(renderer: mock.thumbnails),
            managed: managed
        )
        thumbnails = renderer
        // The viewer's own loader asks PhotoKit first and answers `nil` for
        // anything that is not a system asset — which is everything in this
        // lane, so the viewer showed a spinner and never a photo. The renderer
        // that draws the grid draws the viewer's pixels too.
        mediaLoader = ViewerMediaLoader(
            fallback: renderer,
            metadataSource: Self.composeMetadata(locations: library, managed: managed)
        )
        placeNames = SystemPlaceNameResolver()
        importer = managed.importer

        serverDiscovery = PreviewServerDiscovery(environment: mock)
        credentials = PreviewCredentials(environment: mock)
        firstDeviceEnrollment = PreviewEnrollmentCeremony()

        let records = InMemoryModerationRecords()
        moderationRecords = records
        untrustedOriginPolicy = records

        let consent = ConsentStore()
        consentStore = consent
        diagnostics = DiagnosticsCoordinator(consent: consent)

        alerts = Self.makeAlerts(mock: mock)
        alertResponder = AlertNotificationResponder()
    }

    /// The on-disk managed store's faces: the provider the timeline and the
    /// trash read it through, the picker-driven importer that writes it, the
    /// reader that decodes its files, and the gate its trash opens through.
    struct ManagedLane {
        let provider: ManagedProvider
        let importer: LibraryImporter
        let reader: ManagedMediaReader
        /// The provider's SR1 gate. It challenges nobody: it opens only when
        /// ``composeTrash(library:managed:)``'s composite admits it after the
        /// port-backed gate has been answered, so one prompt opens both trashes
        /// and the managed one never opens on its own — nor through the system
        /// `LAContext` sheet this lane keeps out.
        let trashGate: SecondaryTrashGate
    }

    /// The picker-driven import flow and the provider that reads what it writes.
    ///
    /// The importer writes the on-disk managed store rather than through
    /// ``ImportPort``, which is a scope-based scan/plan/execute flow with no
    /// entry point for a handful of picked files. Nothing here touches the
    /// system photo library — the picker is out-of-process and needs no
    /// authorization. The one `ManagedProvider` built here is shared by the
    /// importer and, through ``composeTimeline(library:managed:)``, the
    /// timeline, which is what makes an import visible once it completes.
    ///
    /// - Parameter openLibrary: the lane's store over the layout. The FFI lane
    ///   supplies one whose writes are the Rust workspace's verbs; with none,
    ///   there is no Rust core, so the store is an in-memory catalog written
    ///   through a `LocalManagedWorkspace`.
    static func makeManagedLane(
        root: URL,
        openLibrary: (@Sendable (ManagedLibraryLayout) -> ManagedLibrary)? = nil
    ) -> ManagedLane {
        let layout = ManagedLibraryLayout(root: root)
        let fileStore = SystemFileStore()
        let library = openLibrary?(layout)
            ?? ManagedLibrary(layout: layout, fileStore: fileStore, catalog: InMemoryAssetCatalog())
        let importService = ImportService(library: library, fileStore: fileStore, hasher: CryptoKitHasher())
        let trashGate = SecondaryTrashGate()
        let provider = ManagedProvider(library: library, authGate: trashGate)
        CapsuleLog.app.debug("managed lane composed at \(root.path, privacy: .private)")
        return ManagedLane(
            provider: provider,
            importer: LibraryImporter(importService: importService, managedProvider: provider),
            reader: ManagedMediaReader(library: library),
            trashGate: trashGate
        )
    }

    /// Recently Deleted over both stores: the composite timeline's `delete`
    /// moves an import to the managed store's trash, and this is where it can
    /// then be listed, restored and purged.
    static func composeTrash(library: any TrashProvider, managed: ManagedLane) -> any TrashProvider {
        CompositeTrashProvider(
            primary: library,
            secondaries: [managed.provider],
            secondaryGate: managed.trashGate
        )
    }

    /// The grid's and the viewer's pixels over both stores.
    ///
    /// The managed provider is asked first: its `nil` is exact (a catalog
    /// lookup), while the mock renderer paints any identifier whose shape it
    /// decodes, which a freshly minted import id can happen to match.
    static func composeThumbnails(library: any ThumbnailProvider, managed: ManagedLane) -> any ThumbnailProvider {
        CompositeThumbnailProvider(providers: [ManagedThumbnailProvider(reader: managed.reader), library])
    }

    /// The info panel's metadata over both stores, the managed file's real EXIF
    /// ahead of the mock's derived fixture for the same ownership reason.
    static func composeMetadata(
        locations: any AssetLocationSource,
        managed: ManagedLane
    ) -> any AssetMetadataSource {
        CompositeMetadataSource(sources: [
            ManagedMetadataSource(reader: managed.reader),
            MockMetadataSource(locations: locations),
        ])
    }

    /// Captions routed to the store that owns the asset, so an import's caption
    /// is never written into the mock library under an id it does not know.
    static func composeCaptions(library: any CaptionStore, managed: ManagedLane) -> any CaptionStore {
        CompositeCaptionStore(primary: library, secondaries: [ManagedCaptionStore(provider: managed.provider)])
    }

    /// The timeline every screen reads: the port-backed library and the
    /// managed store, interleaved by capture date.
    ///
    /// While the managed store is empty the port-backed snapshot passes through
    /// the composite unchanged, with no extra copy or sort. A port-backed load
    /// failure with nothing imported is thrown rather than shown as an empty
    /// library, so the timeline still reaches its failed state.
    static func composeTimeline(library: any AssetProvider, managed: ManagedProvider) -> any AssetProvider {
        CompositeAssetProvider(providers: [library, managed])
    }

    /// The managed library's root, falling back to the temporary directory if
    /// Application Support cannot be located.
    private static func libraryRoot() -> URL {
        if let root = try? ManagedLibraryLayout.defaultRoot() {
            return root
        }
        return URL.temporaryDirectory.appending(path: "CapsuleLibrary", directoryHint: .isDirectory)
    }
}
