import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockSidecarFactory

/// Builds the two signed records behind an asset: its sidecar and its
/// provenance chain.
///
/// Separate from the ``LibraryAsset`` projection on purpose. The projection is
/// what a grid cell binds to; the sidecar is the *wire* record, with the CRDT
/// registers, the superseded log, and the identifiers a view has no business
/// touching. Building them in one place from the same derivation keeps them
/// consistent — a caption in the projection and a caption register that
/// disagreed would make the "restore a displaced caption" flow untestable.
public enum MockSidecarFactory {
    /// The primitive bundle these records claim to have been written under.
    static let cryptoSuiteID: UInt16 = 1
    static let protocolVersion = "2026-05-01"
    static let clientVersion = "capsule-ios/0.1.0"
    static let ownerHandle = "avery@capsule.example"

    /// The signed sidecar for one asset, with the user's edits folded into the
    /// registers they belong to.
    public static func sidecar(
        library: MockLibrary,
        ref: MockAssetRef,
        patch: MockAssetPatch?,
        clock _: MockClock
    ) -> SidecarV1 {
        let seed = library.profile.seed
        let identifier = ref.identifier(seed: seed)
        let derived = library.asset(for: ref)
        let device = MockTagIdentity.authoringDevice(seed: seed)
        let stamp = derived.importTimestamp
        return SidecarV1(
            sidecarSchema: library.isFromNewerVersion(derivationIndex: ref.derivationIndex) ? 2 : 1,
            cryptoSuiteID: cryptoSuiteID,
            uuid: ref.uuidString(seed: seed),
            hash: contentHash(seed: seed, ref: ref),
            captureTimestamp: derived.captureTime.captureTimestamp,
            importTimestamp: derived.importTimestamp,
            contentType: derived.contentType,
            dimensions: derived.dimensions,
            lqip: derived.lqip,
            tagsUser: userTagSet(derived: derived, identifier: identifier, patch: patch, seed: seed),
            tagsAI: aiTagSet(derived: derived, identifier: identifier, patch: patch, seed: seed),
            caption: captionRegister(derived: derived, patch: patch, device: device, stamp: stamp),
            rating: register(value: derived.rating, device: device, stamp: stamp, isWritten: derived.rating > 0),
            stackMembership: stackRegister(derived: derived, device: device, stamp: stamp),
            cull: register(
                value: derived.cull,
                device: device,
                stamp: stamp,
                isWritten: derived.cull != .neutral
            ),
            hidden: register(
                value: derived.isUserHidden,
                device: device,
                stamp: stamp,
                isWritten: derived.isUserHidden
            ),
            cameraID: cameraIdentity(library: library, ref: ref, derived: derived),
            deviceID: device,
            sessionID: MockIdentifiers.sessionID(seed: seed, ordinal: 0),
            gps: patch?.geolocation.applied(to: library.geolocation(for: ref)) ?? library.geolocation(for: ref),
            provenanceChainHash: ref.kind == .live && ref.index == 0 ? nil : chainHash(seed: seed, ref: ref, step: 0),
            unknownFields: unknownFields(library: library, ref: ref)
        )
    }

    /// A register that has only ever been written once, or never at all.
    ///
    /// "Never written" and "written back to the default" are different states,
    /// and the wire-absent defaults depend on the difference — so an unrated
    /// asset gets an empty register rather than one stamped `0`.
    private static func register<Value: Sendable & Equatable>(
        value: Value,
        device: DeviceID,
        stamp: CapsuleTimestamp,
        isWritten: Bool
    ) -> Lww<Value> {
        isWritten ? Lww(current: Stamped(value: value, timestamp: stamp, author: device)) : Lww()
    }

    private static func stackRegister(
        derived: LibraryAsset,
        device: DeviceID,
        stamp: CapsuleTimestamp
    ) -> Lww<StackMembership?> {
        guard let membership = derived.stackMembership else { return Lww() }
        return Lww(current: Stamped(value: membership, timestamp: stamp, author: device))
    }

    /// The caption register, including the log of what it displaced — which is
    /// what the viewer surfaces as "this caption replaced another" and offers to
    /// restore.
    private static func captionRegister(
        derived: LibraryAsset,
        patch: MockAssetPatch?,
        device: DeviceID,
        stamp: CapsuleTimestamp
    ) -> Lww<String> {
        var superseded = patch?.supersededCaptions ?? []
        if derived.hasSupersededCaptions, let current = derived.caption {
            superseded.append(Stamped(
                value: "\(current) (earlier)",
                timestamp: CapsuleTimestamp(epochSeconds: stamp.epochSeconds - 3600),
                author: device
            ))
        }
        let resolved = patch?.caption.applied(to: derived.caption) ?? derived.caption
        guard let resolved else { return Lww(current: nil, superseded: superseded) }
        return Lww(
            current: Stamped(value: resolved, timestamp: stamp, author: device),
            superseded: superseded
        )
    }

    private static func userTagSet(
        derived: LibraryAsset,
        identifier: AssetID,
        patch: MockAssetPatch?,
        seed: UInt64
    ) -> OrSet<String> {
        var adds: [AddID: String] = [:]
        for tag in derived.tagsUser {
            adds[MockTagIdentity.addID(forTag: tag, identifier: identifier, seed: seed, isAI: false)] = tag
        }
        for (addID, tag) in patch?.addedUserTags ?? [:] {
            adds[addID] = tag
        }
        return OrSet(adds: adds, removes: patch?.removedUserTagIDs ?? [])
    }

    private static func aiTagSet(
        derived: LibraryAsset,
        identifier: AssetID,
        patch: MockAssetPatch?,
        seed: UInt64
    ) -> OrSet<AiTag> {
        var adds: [AddID: AiTag] = [:]
        for tag in derived.tagsAI {
            adds[MockTagIdentity.addID(forTag: tag.tag, identifier: identifier, seed: seed, isAI: true)] = tag
        }
        return OrSet(adds: adds, removes: patch?.dismissedAITagIDs ?? [])
    }

    private static func cameraIdentity(
        library: MockLibrary,
        ref: MockAssetRef,
        derived: LibraryAsset
    ) -> CameraID? {
        guard derived.contentType.mediaKind == .image else { return nil }
        let camera = library.camera(derivationIndex: ref.derivationIndex)
        return CameraID(model: camera.model, serial: library.cameraSerial(derivationIndex: ref.derivationIndex))
    }

    /// Bytes from a schema this build does not implement, preserved verbatim and
    /// **never inspected**. Non-empty only for the newer-version population, so
    /// the round-trip-unknown-fields path has something to round-trip.
    private static func unknownFields(library: MockLibrary, ref: MockAssetRef) -> Data {
        guard library.isFromNewerVersion(derivationIndex: ref.derivationIndex) else { return Data() }
        var bytes: [UInt8] = []
        for step in 0 ..< 2 {
            let hash = MockHash.value(seed: library.profile.seed, index: ref.derivationIndex, salt: .schemaAhead, sub: step)
            withUnsafeBytes(of: hash.bigEndian) { bytes.append(contentsOf: $0) }
        }
        return Data(bytes)
    }

    static func contentHash(seed: UInt64, ref: MockAssetRef) -> String {
        MockIdentifiers.blobHash(seed: seed, ordinal: ref.derivationIndex).rawValue
    }

    static func chainHash(seed: UInt64, ref: MockAssetRef, step: Int) -> String {
        MockHash.hex(
            MockHash.value(seed: seed, index: ref.derivationIndex, salt: .identity, sub: 4096 + step),
            digits: 32
        )
    }
}
