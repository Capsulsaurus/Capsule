import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - OrganizePort

extension MockLibraryStore: OrganizePort {
    public func setRating(_ rating: UInt8, for assetIDs: [AssetID]) async throws {
        let clamped = min(5, rating)
        await mutate(assetIDs) { $0.rating = clamped }
    }

    /// Set the culling flag.
    ///
    /// Applied to a collapsed stack it flags **every member**, one edit each,
    /// because a group has no stored flag of its own — its state is derived from
    /// its members every time. A stored group flag would be a second source of
    /// truth that diverges the moment one member is re-flagged.
    public func setCull(_ flag: CullFlag, for assetIDs: [AssetID]) async throws {
        try flag.requireWritable()
        var targets: [AssetID] = []
        for assetID in assetIDs {
            targets.append(assetID)
            guard let ref = MockAssetRef.decode(assetID), ref.kind == .live else { continue }
            targets.append(contentsOf: library.stackRefs(at: ref.index).dropFirst()
                .map { $0.identifier(seed: configuration.seed) })
        }
        await mutate(targets) { $0.cull = flag }
    }

    /// Set the user-hidden flag.
    ///
    /// View-layer only. A hidden asset stays in its album, keeps syncing, and
    /// stays reachable from its stack and from any share it was already part
    /// of — this is neither deletion nor access control, and a UI that implies
    /// otherwise is promising a guarantee the system does not make.
    public func setHidden(_ hidden: Bool, for assetIDs: [AssetID]) async throws {
        await mutate(assetIDs) { $0.isUserHidden = hidden }
        await announceReload()
    }

    public func addUserTag(_ tag: String, to assetIDs: [AssetID]) async throws {
        for assetID in assetIDs {
            let addID = issueAddID()
            await mutate(assetID) { $0.addedUserTags[addID] = tag }
        }
    }

    /// Remove a user tag by the add id that introduced it.
    ///
    /// A remove naming an add this replica never observed is **rejected**, not
    /// ignored — that tolerance is the "remove an element you never added"
    /// attack, and the defence only exists if the rejection is real.
    public func removeUserTag(addID: AddID, from assetID: AssetID) async throws {
        let patch = currentOverlay.patch(for: assetID)
        if patch?.addedUserTags[addID] != nil {
            await mutate(assetID) { $0.addedUserTags[addID] = nil }
            return
        }
        guard let derived = engine.asset(for: assetID),
              MockTagIdentity.userTag(
                  matching: addID,
                  in: derived.tagsUser,
                  identifier: assetID,
                  seed: configuration.seed
              ) != nil
        else { throw UnobservedRemove(addID: addID) }
        await mutate(assetID) { $0.removedUserTagIDs.insert(addID) }
    }

    /// Promote an AI tag to a user tag.
    ///
    /// An explicit user action that **copies** the entry under a fresh
    /// user-scoped add id, into the structurally separate user OR-set. Never
    /// automatic — the separation is what stops a hallucinating model
    /// overwriting user intent, and an automatic promotion would defeat it.
    public func promoteAITag(addID: AddID, on assetID: AssetID, alsoRemoveFromAI: Bool) async throws {
        guard let derived = engine.asset(for: assetID),
              let tag = MockTagIdentity.aiTag(
                  matching: addID,
                  in: derived.tagsAI,
                  identifier: assetID,
                  seed: configuration.seed
              )
        else { throw UnobservedRemove(addID: addID) }
        let userAddID = issueAddID()
        await mutate(assetID) { patch in
            patch.addedUserTags[userAddID] = tag.tag
            if alsoRemoveFromAI { patch.dismissedAITagIDs.insert(addID) }
        }
    }

    public func dismissAITag(addID: AddID, on assetID: AssetID) async throws {
        guard let derived = engine.asset(for: assetID),
              MockTagIdentity.aiTag(
                  matching: addID,
                  in: derived.tagsAI,
                  identifier: assetID,
                  seed: configuration.seed
              ) != nil
        else { throw UnobservedRemove(addID: addID) }
        await mutate(assetID) { $0.dismissedAITagIDs.insert(addID) }
    }

    /// Set the caption.
    ///
    /// The displaced value is kept rather than clobbered, so the viewer can
    /// offer to restore it. A client that dropped the superseded log would be in
    /// violation of the forbidden-client-behaviours rule, and the whole
    /// "this caption replaced another" surface would have nothing to read.
    public func setCaption(_ caption: String?, for assetID: AssetID) async throws {
        let existing = engine.asset(for: assetID)?.caption
        let stamp = Stamped(value: existing ?? "", timestamp: now, author: authoringDevice)
        await mutate(assetID) { patch in
            if let existing, existing != caption {
                patch.supersededCaptions.insert(
                    Stamped(value: existing, timestamp: stamp.timestamp, author: stamp.author),
                    at: 0
                )
            }
            patch.caption = caption.map { MockFieldEdit.set($0) } ?? .cleared
        }
    }

    public func restoreCaption(_ superseded: Stamped<String>, for assetID: AssetID) async throws {
        try await setCaption(superseded.value, for: assetID)
    }

    /// Set or clear the capture coordinate.
    ///
    /// Stored **verbatim in its datum** and never converted at rest: GCJ-02 has
    /// no exact inverse, so converting on input would destroy the user's ground
    /// truth.
    public func setGps(_ gps: Gps?, for assetID: AssetID) async throws {
        if let gps { try gps.datum.requireWritable() }
        await mutate(assetID) { $0.geolocation = gps.map { MockFieldEdit.set($0) } ?? .cleared }
    }
}
