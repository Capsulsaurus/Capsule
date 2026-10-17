import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - Provenance

public extension MockSidecarFactory {
    /// The asset's append-only, hash-chained provenance, oldest first.
    ///
    /// Built as a real chain rather than a list of events: each record's
    /// `prior_provenance_hash` is the previous record's own hash, and the
    /// manifest mirrors it, so ``ProvenanceRecord/mirrorsManifest`` holds for
    /// every link. An activity view that only ever sees well-formed chains
    /// cannot be trusted to render a broken one, so the shape has to be right
    /// even in a mock.
    ///
    /// A trash-restore appends a record and leaves the `delete` in place — the
    /// chain keeps "deleted on X, restored on Y", which is the whole reason no
    /// path exists to remove a record.
    static func provenanceChain(
        library: MockLibrary,
        ref: MockAssetRef,
        patch: MockAssetPatch?,
        clock: MockClock
    ) -> [ProvenanceRecord] {
        let derived = library.asset(for: ref)
        var actions: [ProvenanceAction] = [.create, .derivativeAdd]
        if derived.rating > 0 || derived.cull != .neutral || derived.caption != nil {
            actions.append(.metadataUpdate)
        }
        if patch?.rating != nil || patch?.cull != nil || patch?.caption != .unchanged {
            actions.append(.metadataUpdate)
        }
        if derived.isDeleted { actions.append(.delete) }
        if patch?.isDeleted == false, ref.kind == .trashed { actions.append(.trashRestore) }

        var records: [ProvenanceRecord] = []
        var priorHash: String?
        for (step, action) in actions.enumerated() {
            let record = makeRecord(
                library: library,
                ref: ref,
                link: ChainLink(action: action, step: step, priorHash: priorHash),
                clock: clock
            )
            records.append(record)
            priorHash = record.recordHash
        }
        return records
    }

    /// One link's position in the chain: what happened, where in the sequence,
    /// and what it follows. Bundled so the record builder reads as
    /// `(what, where, when)` rather than six positional arguments.
    struct ChainLink: Sendable {
        var action: ProvenanceAction
        var step: Int
        var priorHash: String?
    }

    private static func makeRecord(
        library: MockLibrary,
        ref: MockAssetRef,
        link: ChainLink,
        clock: MockClock
    ) -> ProvenanceRecord {
        let action = link.action
        let step = link.step
        let priorHash = link.priorHash
        let seed = library.profile.seed
        let derived = library.asset(for: ref)
        let recordHash = chainHash(seed: seed, ref: ref, step: step + 1)
        let core = ManifestCore(
            version: "asset-manifest/v1",
            cryptoSuiteID: cryptoSuiteID,
            protocolVersion: protocolVersion,
            fileID: ref.uuidString(seed: seed),
            albumID: albumText(derived.albumID),
            amkVersion: UInt32(3 + step),
            ciphertextHash: MockIdentifiers.blobHash(seed: seed, ordinal: ref.derivationIndex &+ step),
            plaintextSize: library.byteSize(for: ref, contentType: derived.contentType),
            chunkSize: 1 << 20,
            keyMode: .derived,
            metadataBlobHash: action.bindsMetadataBlob
                ? MockIdentifiers.blobHash(seed: seed, ordinal: ref.derivationIndex &+ 7000 &+ step)
                : nil,
            createdByUser: ownerHandle,
            createdByDevice: MockTagIdentity.authoringDevice(seed: seed),
            clientVersion: clientVersion,
            timestamp: CapsuleTimestamp(
                epochSeconds: derived.importTimestamp.epochSeconds + Int64(step) * 7200
            ),
            action: action,
            priorProvenanceHash: action.isChainRoot ? nil : priorHash,
            retentionUntil: action == .delete ? clock.offset(days: 30) : nil
        )
        return ProvenanceRecord(
            assetID: ref.uuidString(seed: seed),
            manifest: AssetManifest(
                core: core,
                deviceSignature: signature(seed: seed, ref: ref, step: step, salt: 1),
                writeSignature: signature(seed: seed, ref: ref, step: step, salt: 2)
            ),
            priorProvenanceHash: action.isChainRoot ? nil : priorHash,
            recordHash: recordHash
        )
    }

    /// Signature bytes are opaque to this layer.
    ///
    /// Nothing in Swift verifies them — verification is the single
    /// `verify_asset` chokepoint in `capsule-core`, and a second, weaker gate
    /// here would be worse than none. So these are deterministic filler of a
    /// plausible length, and no test should read them.
    private static func signature(seed: UInt64, ref: MockAssetRef, step: Int, salt: Int) -> Data {
        var bytes: [UInt8] = []
        bytes.reserveCapacity(64)
        for word in 0 ..< 8 {
            let hash = MockHash.value(
                seed: seed,
                index: ref.derivationIndex,
                salt: .identity,
                sub: salt &* 100000 &+ step &* 64 &+ word
            )
            withUnsafeBytes(of: hash.bigEndian) { bytes.append(contentsOf: $0) }
        }
        return Data(bytes)
    }

    private static func albumText(_ identifier: AlbumID?) -> String {
        switch identifier {
        case let .managed(uuid): uuid
        case let .smart(localIdentifier): localIdentifier
        case nil: ""
        }
    }
}
