import AssetKit
import CapsuleCatalog
import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsulePorts
import FeatureTimeline
import Foundation
import ImagePipeline
import ManagedStore

// MARK: - The managed lane

/// The on-disk managed store's composition: the lane built over it, and the
/// composites that put it beside the port-backed library on every screen.
extension AppEnvironment {
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
        /// The render ladder the store's pixels are drawn through.
        let renderLadder: RenderLadder
        /// Where a RAW original's embedded preview is read, when the lane can.
        let embeddedPreviews: (any EmbeddedPreviewPort)?
        /// One sync of the store with its server, handing each drain to
        /// ``provider``; `nil` for a lane with no session.
        var sync: ManagedLaneSync?
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
    /// - Parameter uploads: the upload sessions a managed delete redacts the
    ///   deleted asset's in-flight upload from. With none, a delete redacts
    ///   nothing.
    /// - Parameter renderLadder: the lane's render ladder. The default is the
    ///   mock lane's stand-in; the FFI lane passes `RenderLadder.ffi`.
    /// - Parameter embeddedPreviews: where a RAW's embedded preview is read.
    ///   With none, every rung is drawn from the original.
    /// - Parameter openSync: the lane's sync of the store over the layout,
    ///   handing each drain to the provider built here. With none (signed
    ///   out, the mock lane), nothing syncs.
    static func makeManagedLane(
        root: URL,
        openLibrary: (@Sendable (ManagedLibraryLayout) -> ManagedLibrary)? = nil,
        uploads: (any UploadPort)? = nil,
        renderLadder: RenderLadder = MockRenderLadder.ladder,
        embeddedPreviews: (any EmbeddedPreviewPort)? = nil,
        openSync: OpenManagedLaneSync? = nil
    ) -> ManagedLane {
        let layout = ManagedLibraryLayout(root: root)
        let fileStore = SystemFileStore()
        let library = openLibrary?(layout)
            ?? ManagedLibrary(layout: layout, fileStore: fileStore, catalog: InMemoryAssetCatalog())
        let importService = ImportService(library: library, fileStore: fileStore, hasher: CryptoKitHasher())
        let trashGate = SecondaryTrashGate()
        let provider = ManagedProvider(
            library: library,
            authGate: trashGate,
            uploads: uploads,
            purgeDelivery: library.purgeDelivery,
            lifecycleDelivery: library.lifecycleDelivery
        )
        CapsuleLog.app.debug("managed lane composed at \(root.path, privacy: .private)")
        redeliverHeldRecords(
            provider,
            purges: library.purgeDelivery != nil,
            lifecycles: library.lifecycleDelivery != nil
        )
        return ManagedLane(
            provider: provider,
            importer: LibraryImporter(importService: importService, managedProvider: provider),
            reader: ManagedMediaReader(library: library),
            trashGate: trashGate,
            renderLadder: renderLadder,
            embeddedPreviews: embeddedPreviews,
            sync: openSync?(layout, provider)
        )
    }

    /// ``makeManagedLane(root:openLibrary:uploads:renderLadder:embeddedPreviews:openSync:)``
    /// with every lane-supplied part taken from `lane`.
    static func makeManagedLane(root: URL, lane: LanePorts, uploads: any UploadPort) -> ManagedLane {
        makeManagedLane(
            root: root,
            openLibrary: lane.openManagedLibrary,
            uploads: uploads,
            renderLadder: lane.renderLadder,
            embeddedPreviews: lane.embeddedPreviews,
            openSync: lane.openManagedSync
        )
    }

    /// Send the purges the workspace kept, and the deletes and restores of
    /// the assets it still holds, whose delivery failed or was held, once a
    /// lane is composed with somewhere to send them.
    ///
    /// A lane composed with a delivery is a signed-in one, so this is the
    /// retry a relaunch and a sign-in run: each rebuilds the world with the
    /// session. It runs detached, because a launch never waits on the
    /// network; the provider logs each delivery's fate, and a record left
    /// undelivered stays on its chain for the next walk. The walk is
    /// `ManagedProvider.redeliverHeldRecords()`: the purges go first, then the
    /// held assets, one after the other so both read one feed drain, and a
    /// sync's walk asked for meanwhile queues behind it. Each walk lists everything the workspace holds of its kind, but an
    /// asset whose head the feed already lists sends nothing, so the cost is
    /// one feed drain plus what the server still lacks. With no delivery
    /// (signed out, the mock lane) there is nothing to send.
    private static func redeliverHeldRecords(_ provider: ManagedProvider, purges: Bool, lifecycles: Bool) {
        guard purges || lifecycles else { return }
        CapsuleLog.app.info(
            "redelivering held records for the signed-in lane, purges=\(purges, privacy: .public) lifecycles=\(lifecycles, privacy: .public)"
        )
        Task {
            await provider.redeliverHeldRecords()
        }
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
        let managedThumbnails = ManagedThumbnailProvider(
            reader: managed.reader,
            ladder: managed.renderLadder,
            embeddedPreviews: managed.embeddedPreviews
        )
        return CompositeThumbnailProvider(providers: [managedThumbnails, library])
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
}

/// One sync of the managed lane with its server. It logs its own failure and
/// keeps what the pages before it applied, so a caller has nothing to handle.
typealias ManagedLaneSync = @Sendable () async -> Void

/// A lane's sync of the managed store at a layout, handing each drain to the
/// provider that reads the store.
typealias OpenManagedLaneSync = @Sendable (ManagedLibraryLayout, ManagedProvider) -> ManagedLaneSync
