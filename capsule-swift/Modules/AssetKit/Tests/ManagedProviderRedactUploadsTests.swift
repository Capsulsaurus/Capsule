import Foundation
import Testing

import AssetKit
import CapsuleCatalog
import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import CapsuleTestSupport
import ManagedStore

/// A managed delete redacts the deleted asset's in-flight upload once the
/// workspace holds the delete, and a redaction that fails never fails the
/// delete (Organization — Deletion State Machine).
@Suite("ManagedProvider.delete redacts uploads")
struct ManagedProviderRedactUploadsTests {
    /// Records each delete; refuses the ids it is told to.
    private actor RecordingWorkspace: ManagedWorkspace {
        private(set) var deleted: [String] = []
        private let refused: Set<String>

        init(refusing refused: Set<String> = []) {
            self.refused = refused
        }

        func sealAsset(fileName _: String, bytes _: Data) -> String { "sealed" }

        func deleteAsset(id: String, retainDays _: UInt32) throws {
            if refused.contains(id) { throw TrashRefusal.notFound }
            deleted.append(id)
        }

        func restoreAsset(id _: String) {}

        func purgeAsset(id: String) -> ManagedPurgeOutcome {
            ManagedPurgeOutcome(assetID: id, reissueRecord: nil)
        }
    }

    /// This device's sessions; cancels the way the server does, and can be made
    /// to fail the session listing outright.
    private actor StubUploads: UploadPort {
        private(set) var sessions: [UploadSession]
        private let listingError: CapsuleError?

        init(sessions: [UploadSession], listingError: CapsuleError? = nil) {
            self.sessions = sessions
            self.listingError = listingError
        }

        func activeSessions() throws -> [UploadSession] {
            if let listingError { throw listingError }
            return sessions
        }

        func uploadPolicy() -> UploadPolicy { .full }
        func setUploadPolicy(_: UploadPolicy) {}
        func forceUpload(assetIDs _: [AssetID]) {}

        func cancelSession(_ id: UploadID) throws {
            guard let session = sessions.first(where: { $0.id == id }) else {
                throw CapsuleError(code: .uploadSessionNotFound)
            }
            guard session.state != .waitingForProcessing else {
                throw CapsuleError(code: .uploadFinalizeInProgress)
            }
            sessions.removeAll { $0.id == id }
        }

        func custodyReceipts(for _: AssetID) -> [CustodyReceipt] { [] }
        nonisolated func changes() -> AsyncStream<[UploadSession]> { AsyncStream { $0.finish() } }
    }

    private static func session(_ id: String, asset: String, state: UploadSessionState) -> UploadSession {
        UploadSession(
            id: UploadID(id),
            assetID: asset,
            blobRole: .original,
            tier: .original,
            state: state,
            offset: 0,
            declaredSize: 8,
            ciphertextHash: BlobHash("hash-\(id)")
        )
    }

    private func makeProvider(_ workspace: RecordingWorkspace, uploads: StubUploads) -> ManagedProvider {
        ManagedProvider(
            library: ManagedLibrary(
                layout: ManagedLibraryLayout(root: URL(filePath: "/capsule/Library")),
                fileStore: MockFileStore(),
                catalog: MockCatalog(),
                workspace: workspace
            ),
            authGate: MockLocalAuthGate(),
            uploads: uploads
        )
    }

    @Test("a delete cancels the deleted asset's sessions and leaves another asset's alone")
    func deleteCancelsOnlyThatAsset() async throws {
        let workspace = RecordingWorkspace()
        let uploads = StubUploads(sessions: [
            Self.session("open", asset: "deleted", state: .uploading),
            Self.session("pending", asset: "deleted", state: .pending),
            Self.session("final", asset: "deleted", state: .waitingForProcessing),
            Self.session("other", asset: "kept", state: .uploading),
        ])

        try await makeProvider(workspace, uploads: uploads).delete([.managed(uuid: "deleted")])

        #expect(await workspace.deleted == ["deleted"])
        // The finalizing session cannot be cancelled; the delete supersedes it.
        #expect(await uploads.sessions.map(\.id) == [UploadID("final"), UploadID("other")])
    }

    @Test("a redaction that fails is swallowed: the delete stands and the next asset is still deleted")
    func failedRedactionDoesNotFailTheDelete() async throws {
        let workspace = RecordingWorkspace()
        let uploads = StubUploads(
            sessions: [Self.session("open", asset: "a", state: .uploading)],
            listingError: CapsuleError(code: .uploadSessionNotFound)
        )

        try await makeProvider(workspace, uploads: uploads).delete([.managed(uuid: "a"), .managed(uuid: "b")])

        #expect(await workspace.deleted == ["a", "b"])
        #expect(await uploads.sessions.count == 1)
    }

    @Test("a delete the workspace refuses redacts nothing")
    func refusedDeleteRedactsNothing() async {
        let workspace = RecordingWorkspace(refusing: ["deleted"])
        let uploads = StubUploads(sessions: [Self.session("open", asset: "deleted", state: .uploading)])
        let provider = makeProvider(workspace, uploads: uploads)

        await #expect(throws: TrashRefusal.notFound) {
            try await provider.delete([.managed(uuid: "deleted")])
        }
        #expect(await uploads.sessions.count == 1)
    }

    @Test("a system Photos id is neither deleted here nor redacted")
    func foreignIdIsSkipped() async throws {
        let workspace = RecordingWorkspace()
        let uploads = StubUploads(sessions: [Self.session("open", asset: "PK/L0/001", state: .uploading)])

        try await makeProvider(workspace, uploads: uploads).delete([.photoKit(localIdentifier: "PK/L0/001")])

        #expect(await workspace.deleted.isEmpty)
        #expect(await uploads.sessions.count == 1)
    }
}
