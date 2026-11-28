@testable import CapsuleCatalogFFI
import Foundation

/// The builders the ``FFISyncDriver`` suites script their feeds and drivers
/// with: entries, tombstones, pages, facts, and the driver under test.
extension FFISyncDriverFixture {
    /// The metadata an opened entry carries.
    static let metadataFacts = FfiAssetMetadata(
        sidecarSchema: 1,
        contentType: "image/jpeg",
        captureTimestamp: "2026-01-01T00:00:00Z",
        width: 4,
        height: 3,
        lqipChromahash: nil,
        lqipDominantColor: nil,
        caption: nil,
        tags: [],
        rating: nil,
        hidden: false
    )

    /// The facts the double answers an applied entry with: its provenance head is
    /// the record hash of `manifest`, as the chokepoint's is.
    static func facts(
        _ manifest: Data,
        action: String = "create",
        metadata: FfiAssetMetadata? = nil,
        adopted: Bool = false
    ) -> FfiAssetFacts {
        FfiAssetFacts(
            assetId: assetA,
            albumId: assetA,
            amkVersion: 1,
            ciphertextHash: originalHash,
            plaintextSize: 1,
            action: action,
            timestamp: "2026-01-01T00:00:00Z",
            createdByDevice: assetA,
            provenanceHead: FFIPurgeDelivery.headHash(manifest),
            metadata: metadata,
            adopted: adopted
        )
    }

    /// A feed entry listing `asset` with `record` as its latest record, carrying
    /// an original the server holds unless `held` is false. As the server's feed
    /// does, it names its metadata blob (`metadataAddress`, when `metadata` is
    /// set) by address only, and lists only the original among its blobs.
    static func entry(
        _ asset: String,
        record: Data,
        kind: FfiChangeKind = .created,
        metadata: Bool = false,
        held: Bool = true,
        metadataAddress: Data = Data(metadataHash.utf8)
    ) -> FfiFeedEntry {
        FfiFeedEntry(
            albumId: Data(repeating: 0xAB, count: 16),
            syncSeq: 1,
            protocolVersion: "2026-01-01",
            kind: kind,
            assetId: Data(asset.utf8),
            manifestCbor: record,
            metadataBlob: metadata ? metadataAddress : Data(),
            blobs: FfiBlobManifest(
                original: FfiBlobRef(ciphertextHash: originalHash, role: "original", size: 64),
                derivatives: []
            ),
            originalHeld: held,
            changedAt: "2026-01-01T00:00:00Z"
        )
    }

    /// The `changed_at` every scripted tombstone carries.
    static let tombstoneChangedAt = "2026-02-01T00:00:00Z"

    /// An album page's tombstone of `asset`: its delete `record`, and, as every
    /// album page serves a delete, no metadata blob, no blob and no held original.
    static func tombstone(_ asset: String, record: Data) -> FfiFeedEntry {
        FfiFeedEntry(
            albumId: Data(repeating: 0xAB, count: 16),
            syncSeq: 1,
            protocolVersion: "2026-01-01",
            kind: .deleted,
            assetId: Data(asset.utf8),
            manifestCbor: record,
            metadataBlob: Data(),
            blobs: FfiBlobManifest(original: nil, derivatives: []),
            originalHeld: false,
            changedAt: tombstoneChangedAt
        )
    }

    static let liveMetadataHash = String(repeating: "e", count: 64)
    static let liveOriginalHash = String(repeating: "f", count: 64)

    /// The blobs a tombstone's history names: its asset's live metadata blob and
    /// the original its delete commits to, neither of which the feed lists.
    static let liveSources = FfiHistorySources(
        metadataBlobHash: liveMetadataHash,
        original: FfiBlobRef(ciphertextHash: liveOriginalHash, role: "original", size: 80)
    )

    static func page(_ entries: [FfiFeedEntry], next: Data, more: Bool = false) -> FfiSyncPage {
        FfiSyncPage(entries: entries, nextCursor: next, hasMore: more)
    }

    static func stagingDirectory() -> URL {
        URL.temporaryDirectory.appending(path: "ffi-sync-\(UUID().uuidString)", directoryHint: .isDirectory)
    }

    static func driver(
        _ verbs: ScriptedVerbs,
        store: MemoryStore = MemoryStore(),
        catalog: RecordingCatalog = RecordingCatalog(),
        staging: URL = stagingDirectory(),
        sink: DrainSink
    ) -> FFISyncDriver {
        FFISyncDriver(verbs: verbs, store: store, stagingDirectory: staging, catalog: catalog) { await sink.record($0) }
    }

    /// The files left in `directory`, hidden ones included.
    static func leftovers(in directory: URL) throws -> [URL] {
        try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
    }
}
