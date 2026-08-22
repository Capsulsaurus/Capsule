import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - UploadPort

extension MockTransferStore: UploadPort {
    /// This device's active sessions.
    ///
    /// Re-derived from server truth on resume in the real system — the local
    /// work queue is a rebuildable cache, never the source of truth — which is
    /// why this reads a list rather than a persisted queue.
    public func activeSessions() async throws -> [UploadSession] {
        currentSessions
    }

    public func uploadPolicy() async throws -> UploadPolicy {
        currentPolicy
    }

    /// Set the upload policy.
    ///
    /// Client-side session **ordering** only. The server has no mode branch to
    /// switch, so this changes when sessions open and nothing else.
    public func setUploadPolicy(_ policy: UploadPolicy) async throws {
        try policy.requireWritable()
        setPolicy(policy)
        await uploadChanges.send(currentSessions)
    }

    /// Force a transfer regardless of the metered and Wi-Fi criteria, on the
    /// user's explicit consent.
    public func forceUpload(assetIDs: [AssetID]) async throws {
        try await behaviourGate.admit()
        let updated = currentSessions.map { session -> UploadSession in
            var session = session
            guard session.state.isCancellable else { return session }
            session.state = .uploading
            session.offset = session.declaredSize
            return session
        }
        setSessions(updated)
        await uploadChanges.send(updated)
    }

    /// Cancel a session.
    ///
    /// **Refused once finalization has begun.** Finalization is not
    /// interruptible — the server is verifying the hash it recomputed — so a
    /// cancel arriving then is rejected rather than half-honoured.
    public func cancelSession(_ identifier: UploadID) async throws {
        guard let session = currentSessions.first(where: { $0.id == identifier }) else {
            throw CapsuleError(code: .uploadSessionNotFound, detail: "CapsuleMock: no such session")
        }
        guard session.state.isCancellable else {
            throw CapsuleError(
                code: .uploadFinalizeInProgress,
                detail: "CapsuleMock: finalization has begun and is not interruptible"
            )
        }
        setSessions(currentSessions.filter { $0.id != identifier })
        await uploadChanges.send(currentSessions)
    }

    /// The custody receipts held for an asset.
    ///
    /// The evidence half of verify-before-destroy: the envelope proves what a
    /// client claimed and signed, the receipt proves what the server accepted
    /// over a hash **it recomputed itself**. Without them, "the server lost my
    /// photo" and "the client never uploaded it" are symmetric unfalsifiable
    /// claims.
    public func custodyReceipts(for assetID: AssetID) async throws -> [CustodyReceipt] {
        guard let ref = MockAssetRef.decode(assetID), store.library.contains(ref) else { return [] }
        guard let asset = await store.engine.asset(for: assetID), asset.syncState == .durable else {
            return []
        }
        return [BlobRole.original, .metadata, .provenance].enumerated().map { position, role in
            receipt(ref: ref, role: role, position: position)
        }
    }

    public nonisolated func changes() -> AsyncStream<[UploadSession]> {
        uploadChanges.subscribe()
    }

    private func receipt(ref: MockAssetRef, role: BlobRole, position: Int) -> CustodyReceipt {
        let seed = configuration.seed
        let ordinal = ref.derivationIndex &* 4 &+ position
        return CustodyReceipt(
            version: "custody-receipt/v1",
            cryptoSuiteID: MockSidecarFactory.cryptoSuiteID,
            protocolVersion: MockSidecarFactory.protocolVersion,
            serverID: "capsule.example",
            serverKeyID: Data(MockHash.hex(MockHash.mix(seed), digits: 16).utf8),
            // Strictly monotonic per server: holding sequence N proves the log
            // has at least N entries, which is what bounds silent truncation.
            receiptSequence: UInt64(100_000 + ordinal),
            priorReceiptHash: ordinal == 0 ? nil : MockSidecarFactory.chainHash(seed: seed, ref: ref, step: position),
            uploadID: MockIdentifiers.uploadID(seed: seed, ordinal: ordinal),
            assetID: ref.uuidString(seed: seed),
            blobRole: role,
            ciphertextHash: MockIdentifiers.blobHash(seed: seed, ordinal: ordinal),
            size: store.library.byteSize(for: ref, contentType: store.library.contentType(for: ref)),
            envelopeHash: MockSidecarFactory.chainHash(seed: seed, ref: ref, step: 1),
            uploadedByUser: MockSidecarFactory.ownerHandle,
            uploadedByDevice: MockTagIdentity.authoringDevice(seed: seed),
            receivedAt: configuration.clock.offset(days: -2),
            serverSignature: Data(MockHash.hex(MockHash.mix(seed &+ UInt64(ordinal)), digits: 16).utf8)
        )
    }
}
