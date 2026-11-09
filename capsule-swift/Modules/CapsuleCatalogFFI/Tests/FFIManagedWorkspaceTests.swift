import CapsuleCatalog
import CapsuleDomain
import Foundation
import Testing

@testable import CapsuleCatalogFFI

/// A workspace double: records every verb it receives and throws `failure`
/// from the trash verbs when one is set.
private final class RecordingVerbs: FFIManagedWorkspaceVerbs, @unchecked Sendable {
    enum Call: Equatable {
        case seal(album: String, fileName: String, bytes: Data)
        case delete(String, UInt32)
        case restore(String)
        case purge(String)
    }

    private let lock = NSLock()
    private var recorded: [Call] = []
    var failure: (any Error)?
    var reissue: Data?

    var calls: [Call] {
        lock.withLock { recorded }
    }

    private func record(_ call: Call, failing: Bool) throws {
        try lock.withLock {
            recorded.append(call)
            if failing, let failure { throw failure }
        }
    }

    func defaultAlbumId() -> String {
        "album-default"
    }

    func sealAsset(albumId: String, fileName: String, bytes: Data) throws -> String {
        try record(.seal(album: albumId, fileName: fileName, bytes: bytes), failing: false)
        return "sealed-1"
    }

    func deleteAsset(assetId: String, retainDays: UInt32) throws -> FfiTrashEntry {
        try record(.delete(assetId, retainDays), failing: true)
        return FfiTrashEntry(
            assetId: assetId,
            deletedAt: "2024-07-03T12:26:40Z",
            retentionUntil: "2024-08-02T12:26:40Z",
            restorable: true
        )
    }

    func restoreAsset(assetId: String) throws {
        try record(.restore(assetId), failing: true)
    }

    func purgeAsset(assetId: String) throws -> FfiPurgeOutcome {
        try record(.purge(assetId), failing: true)
        return FfiPurgeOutcome(assetId: assetId, reissueRecord: reissue)
    }
}

private func trashRefusal(_ refusal: FfiTrashRefusal, retentionUntil: String? = nil) -> FfiError {
    .Trash(refusal: refusal, code: nil, retentionUntil: retentionUntil, detail: "refused")
}

@Suite("FFIManagedWorkspace — the workspace's import and trash verbs as the ManagedWorkspace")
struct FFIManagedWorkspaceTests {
    @Test("an import is sealed into the default album under its own name and bytes")
    func sealsIntoTheDefaultAlbum() async throws {
        let verbs = RecordingVerbs()
        let workspace = FFIManagedWorkspace(source: verbs)

        let id = try await workspace.sealAsset(fileName: "IMG_0001.HEIC", bytes: Data("photo".utf8))

        #expect(id == "sealed-1")
        #expect(verbs.calls == [.seal(album: "album-default", fileName: "IMG_0001.HEIC", bytes: Data("photo".utf8))])
    }

    @Test("delete, restore and purge reach the workspace verbs, and a purge hands back the re-issue")
    func trashVerbsReachTheWorkspace() async throws {
        let verbs = RecordingVerbs()
        verbs.reissue = Data([0xA1, 0x00])
        let workspace = FFIManagedWorkspace(source: verbs)

        try await workspace.deleteAsset(id: "a", retainDays: 30)
        try await workspace.restoreAsset(id: "a")
        let outcome = try await workspace.purgeAsset(id: "a")

        #expect(outcome == ManagedPurgeOutcome(assetID: "a", reissueRecord: Data([0xA1, 0x00])))
        #expect(verbs.calls == [.delete("a", 30), .restore("a"), .purge("a")])
    }

    @Test(
        "each trash refusal becomes the TrashRefusal it names",
        arguments: [
            (FfiTrashRefusal.notFound, nil as String?, TrashRefusal.notFound),
            (FfiTrashRefusal.notInTrash, nil, TrashRefusal.notInTrash),
            (
                FfiTrashRefusal.retentionElapsed,
                "2024-08-02T12:26:40Z",
                TrashRefusal.retentionElapsed(retentionUntil: "2024-08-02T12:26:40Z")
            ),
        ]
    )
    func translatesRefusals(refusal: FfiTrashRefusal, until: String?, expected: TrashRefusal) async {
        let verbs = RecordingVerbs()
        verbs.failure = trashRefusal(refusal, retentionUntil: until)
        let workspace = FFIManagedWorkspace(source: verbs)

        await #expect(throws: expected) { try await workspace.deleteAsset(id: "a", retainDays: 30) }
        await #expect(throws: expected) { try await workspace.restoreAsset(id: "a") }
        await #expect(throws: expected) { _ = try await workspace.purgeAsset(id: "a") }
    }

    @Test("a failure that is not a trash refusal is rethrown as it came")
    func otherFailuresPassThrough() async {
        let verbs = RecordingVerbs()
        verbs.failure = FfiError.Workspace(detail: "disk full")
        let workspace = FFIManagedWorkspace(source: verbs)

        await #expect(throws: FfiError.self) { try await workspace.restoreAsset(id: "a") }
    }

    @Test("a trash entry carries the workspace's window and its restorability")
    func trashEntryCarriesTheJudgement() throws {
        let entry = try #require(TrashEntry(FfiTrashEntry(
            assetId: "a",
            deletedAt: "2024-07-03T12:26:40Z",
            retentionUntil: "2024-08-02T12:26:40Z",
            restorable: false
        )))

        #expect(entry.assetID == "a")
        #expect(entry.deletedAt == CapsuleTimestamp(epochSeconds: 1720009600))
        #expect(entry.retentionUntil == CapsuleTimestamp(rfc3339: "2024-08-02T12:26:40Z"))
        #expect(!entry.isRestorable)
        #expect(TrashEntry(FfiTrashEntry(assetId: "a", deletedAt: "2024-07-03T12:26:40Z", retentionUntil: nil, restorable: true))?
            .retentionUntil == nil)
        #expect(TrashEntry(FfiTrashEntry(assetId: "a", deletedAt: "yesterday", retentionUntil: nil, restorable: true)) == nil)
    }
}
