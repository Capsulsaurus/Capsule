import AssetKit
import CapsuleCatalog
import CapsuleDiagnostics
import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsulePorts
import FeatureTimeline
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
    /// quarantine` and an Xcode scheme argument select the same world.
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
    let importer: LibraryImporter

    /// Persisted diagnostics & telemetry consent (local-only by default).
    let consentStore: ConsentStore
    /// Wires MetricKit, breadcrumbs, the crash prompt, and bug-report export.
    let diagnostics: DiagnosticsCoordinator

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

    var importing: any ImportPort { mock.importing }
    var uploads: any UploadPort { mock.uploads }
    var sync: any SyncPort { mock.sync }
    var storage: any StoragePort { mock.storage }
    var quota: any QuotaPort { mock.quota }

    var auth: any AuthPort { mock.auth }
    var devices: any DevicePort { mock.devices }
    var enrollment: any EnrollmentPort { mock.enrollment }
    var recovery: any RecoveryPort { mock.recovery }

    var quarantine: any QuarantinePort { mock.quarantine }
    var maintenance: any MaintenancePort { mock.maintenance }
    var settings: any SettingsPort { mock.settings }

    var federation: any FederationPort { mock.federation }
    var peering: any PeeringPort { mock.peering }
    var moderation: any ModerationPort { mock.moderation }
    var sharing: any SharePort { mock.sharing }
    var drops: any DropPort { mock.drops }

    // MARK: Construction

    init(mock: MockEnvironment = .fromLaunchArguments()) {
        self.mock = mock
        CapsuleLog.app.info("mock scenario: \(mock.scenario.rawValue, privacy: .public)")

        assetProvider = PortBackedAssetProvider(library: mock.library, organize: mock.organize)
        albumProvider = PortBackedAlbumProvider(albums: mock.albums, library: mock.library)
        trashProvider = PortBackedTrashProvider(organize: mock.organize, library: mock.library)
        hiddenStore = HiddenStore()
        thumbnails = PortBackedThumbnailProvider(renderer: mock.thumbnails)
        mediaLoader = ViewerMediaLoader()
        importer = Self.makeImporter()

        let consent = ConsentStore()
        consentStore = consent
        diagnostics = DiagnosticsCoordinator(consent: consent)
    }

    /// The picker-driven import flow.
    ///
    /// Still writing into the on-disk managed store rather than through
    /// ``ImportPort``: `LibraryImporter` is declared in `FeatureTimeline` and
    /// takes a `ManagedProvider` by constructor, so re-pointing it at the port
    /// is a change to that module rather than to this one. Nothing here touches
    /// the system photo library — the picker is out-of-process and needs no
    /// authorization — but until the importer is re-pointed, an imported file
    /// lands in the managed catalog that the mock timeline does not read.
    private static func makeImporter() -> LibraryImporter {
        let layout = ManagedLibraryLayout(root: libraryRoot())
        let library = ManagedLibrary(layout: layout, catalogOpener: InMemoryCatalogOpener())
        let importService = ImportService(
            library: library,
            fileStore: SystemFileStore(),
            hasher: CryptoKitHasher(),
            metadataExtractor: ImageIOMetadataExtractor(),
            sidecarCoder: JSONSidecarCoder()
        )
        return LibraryImporter(
            importService: importService,
            managedProvider: ManagedProvider(library: library)
        )
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
