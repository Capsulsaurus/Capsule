import CapsuleCatalog
import CapsuleFoundation
import Foundation

/// The Capsule-managed, on-disk photo library.
///
/// Owns the library's location (``ManagedLibraryLayout``) and lazily opens the
/// catalog, ensuring the directory skeleton exists first. The import pipeline
/// and the managed provider share one instance so they read and write a single
/// catalog.
///
/// *How* the catalog is opened is injected as ``CatalogOpening`` rather than
/// hard-wired: a build that links the Rust core passes an opener over
/// `FFIAssetCatalog`, while the mock lane, previews, and unit tests pass an
/// in-memory one. This module therefore compiles with no Rust core present.
///
/// The catalog only reads. Every write — an import, a delete, a restore, a
/// purge — goes through ``workspace``: on the FFI lane that is the Rust
/// workspace over the same root, whose signed records the catalog's rows
/// project; elsewhere it is a ``LocalManagedWorkspace`` over an in-memory
/// catalog.
public actor ManagedLibrary {
    /// The library's on-disk layout.
    public let layout: ManagedLibraryLayout

    /// The library's writes: sealing an import and every trash transition.
    public nonisolated let workspace: any ManagedWorkspace

    private let fileStore: any FileStore
    private let catalogOpener: any CatalogOpening
    private var openedCatalog: (any AssetCatalog)?

    /// A library whose catalog is opened lazily by `catalogOpener` and whose
    /// writes go through `workspace` — the FFI lane, where both sit over the
    /// same root.
    public init(
        layout: ManagedLibraryLayout,
        fileStore: any FileStore = SystemFileStore(),
        catalogOpener: any CatalogOpening,
        workspace: any ManagedWorkspace
    ) {
        self.layout = layout
        self.fileStore = fileStore
        self.catalogOpener = catalogOpener
        self.workspace = workspace
    }

    /// A library over an in-memory catalog, written through a
    /// ``LocalManagedWorkspace`` built over the same catalog and file store —
    /// the mock lane, previews and tests. The catalog is already open, so the
    /// lazy open is skipped.
    public init(
        layout: ManagedLibraryLayout,
        fileStore: any FileStore,
        catalog: InMemoryAssetCatalog,
        hasher: any ContentHasher = CryptoKitHasher(),
        metadataExtractor: any MediaMetadataExtracting = ImageIOMetadataExtractor(),
        sidecarCoder: any SidecarCoding = JSONSidecarCoder()
    ) {
        self.layout = layout
        self.fileStore = fileStore
        catalogOpener = PreopenedCatalogOpener(catalog: catalog)
        openedCatalog = catalog
        workspace = LocalManagedWorkspace(
            layout: layout,
            fileStore: fileStore,
            catalog: catalog,
            hasher: hasher,
            metadataExtractor: metadataExtractor,
            sidecarCoder: sidecarCoder
        )
    }

    /// A library over an already-open catalog and an injected workspace —
    /// for tests that observe what reaches the workspace.
    public init(
        layout: ManagedLibraryLayout,
        fileStore: any FileStore,
        catalog: any AssetCatalog,
        workspace: any ManagedWorkspace
    ) {
        self.layout = layout
        self.fileStore = fileStore
        catalogOpener = PreopenedCatalogOpener(catalog: catalog)
        openedCatalog = catalog
        self.workspace = workspace
    }

    /// The catalog, opening it (and creating the directory skeleton) on first
    /// use. Subsequent calls return the already-open catalog.
    public func catalog() async throws -> any AssetCatalog {
        if let openedCatalog {
            return openedCatalog
        }
        // Bound to a local first: the os_log interpolation is an autoclosure, so
        // reading `layout` inline would require an explicit `self.` capture that
        // the formatter strips back out.
        let rootPath = layout.root.path
        CapsuleLog.managedStore.info("preparing managed library at \(rootPath, privacy: .public)")
        for directory in layout.skeletonDirectories {
            try await fileStore.createDirectory(at: directory)
        }
        let catalog = try catalogOpener.openCatalog(at: layout.catalogFile)
        openedCatalog = catalog
        CapsuleLog.managedStore.info("managed library catalog opened")
        return catalog
    }

    /// The media file an asset's bytes live in, or `nil` when none is on disk.
    ///
    /// Found by stem: the catalog does not record the extension the importer
    /// derived. The paired `.cbor` sidecar shares the stem and is skipped, so the
    /// answer is always the original, never its record.
    ///
    /// - Parameter captureDate: the asset's capture instant, which decides its
    ///   `media/{YYYY}/{YYYY-MM}` partition.
    public func mediaFile(uuid: String, captureDate: Date) async throws -> URL? {
        let directory = layout.mediaDirectory(forCaptureDate: captureDate)
        guard await fileStore.fileExists(at: directory) else { return nil }
        return try await fileStore.contentsOfDirectory(at: directory).first { entry in
            entry.deletingPathExtension().lastPathComponent == uuid
                && entry.pathExtension.lowercased() != Self.sidecarExtension
        }
    }

    /// Rewrite an asset's sidecar: decode it through `coder`, apply `change`, and
    /// write the result back in place.
    ///
    /// Read, change and write all happen here so the fields the change does not
    /// touch — ``CatalogSidecar/unknownFieldsCBOR`` included — survive verbatim.
    ///
    /// - Parameter captureDate: the asset's capture instant, which decides its
    ///   `media/{YYYY}/{YYYY-MM}` partition.
    /// - Throws: ``CatalogError/sidecar(message:)`` when the asset has no sidecar
    ///   on disk or it does not decode; the file store's error when the read or
    ///   the write fails. Nothing is written unless the whole change is.
    public func updateSidecar(
        uuid: String,
        captureDate: Date,
        coder: any SidecarCoding,
        _ change: @Sendable (inout CatalogSidecar) -> Void
    ) async throws {
        let url = layout.sidecarFile(uuid: uuid, captureDate: captureDate)
        guard await fileStore.fileExists(at: url) else {
            CapsuleLog.managedStore.error("sidecar update failed for \(uuid, privacy: .public): no sidecar on disk")
            throw CatalogError.sidecar(message: "no sidecar for \(uuid)")
        }
        var sidecar = try await coder.decode(fileStore.read(at: url))
        change(&sidecar)
        try await fileStore.write(coder.encode(sidecar), to: url)
        CapsuleLog.managedStore.debug("sidecar updated for \(uuid, privacy: .public)")
    }

    /// The extension every sidecar is written under; see ``ManagedLibraryLayout``.
    private static let sidecarExtension = "cbor"
}
