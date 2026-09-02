import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockQueryEngine

/// Evaluates a ``TimelineQuery`` against the derived library plus the user's
/// edits.
///
/// A value type over a snapshot of both, so the store can hand one out and let
/// a 250 000-index scan run without holding its actor. It is also the single
/// place the paged read and the day aggregate are computed, which is what makes
/// them agree by construction: `dayCounts(matching:)` and
/// `page(matching:offset:limit:)` walk the *same* enumeration in the same order,
/// one counting and one collecting.
public struct MockQueryEngine: Sendable {
    public let library: MockLibrary
    public let overlay: MockOverlay
    /// The injected clock. Nothing here reads `Date()`.
    public let now: CapsuleTimestamp

    public init(library: MockLibrary, overlay: MockOverlay, now: CapsuleTimestamp) {
        self.library = library
        self.overlay = overlay
        self.now = now
    }

    private var seed: UInt64 { library.profile.seed }

    // MARK: Single asset

    /// Resolve one asset, or `nil` when it no longer exists.
    ///
    /// A purged asset resolves to `nil` while its provenance chain survives —
    /// the tombstone-with-history rule — so the activity view still answers
    /// "what happened to this photo" for a photo that is gone.
    public func asset(for identifier: AssetID) -> LibraryAsset? {
        guard let ref = MockAssetRef.decode(identifier), library.contains(ref) else { return nil }
        guard overlay.patch(for: identifier)?.isPurged != true else { return nil }
        return patched(library.asset(for: ref), identifier: identifier)
    }

    /// Apply the user's edits over a derived asset.
    public func patched(_ derived: LibraryAsset, identifier: AssetID) -> LibraryAsset {
        guard let patch = overlay.patch(for: identifier) else { return derived }
        var asset = derived
        if let rating = patch.rating { asset.rating = rating }
        if let cull = patch.cull { asset.cull = cull }
        if let hidden = patch.isUserHidden { asset.isUserHidden = hidden }
        if let deleted = patch.isDeleted {
            asset.isDeleted = deleted
            asset.deletedAt = deleted ? patch.deletedAt : nil
        }
        if let albumID = patch.albumID { asset.albumID = albumID }
        if case .unchanged = patch.caption {} else {
            asset.caption = patch.caption.applied(to: derived.caption)
        }
        asset.hasSupersededCaptions = derived.hasSupersededCaptions || !patch.supersededCaptions.isEmpty
        if case .unchanged = patch.stackMembership {} else {
            asset.stackMembership = patch.stackMembership.applied(to: derived.stackMembership)
            asset.isStackHidden = asset.stackMembership?.isStackCover == false
        }
        if let ladder = patch.representations { asset.representations = ladder }
        if let state = patch.syncState { asset.syncState = state }
        asset.tagsUser = editedUserTags(derived: derived, identifier: identifier, patch: patch)
        asset.tagsAI = editedAITags(derived: derived, identifier: identifier, patch: patch)
        return asset
    }

    private func editedUserTags(
        derived: LibraryAsset,
        identifier: AssetID,
        patch: MockAssetPatch
    ) -> Set<String> {
        var tags = derived.tagsUser
        for addID in patch.removedUserTagIDs {
            if let tag = MockTagIdentity.userTag(matching: addID, in: derived.tagsUser, identifier: identifier, seed: seed) {
                tags.remove(tag)
            }
        }
        tags.formUnion(patch.addedUserTags.values)
        return tags
    }

    private func editedAITags(
        derived: LibraryAsset,
        identifier: AssetID,
        patch: MockAssetPatch
    ) -> Set<AiTag> {
        guard !patch.dismissedAITagIDs.isEmpty else { return derived.tagsAI }
        var tags = derived.tagsAI
        for addID in patch.dismissedAITagIDs {
            if let tag = MockTagIdentity.aiTag(matching: addID, in: derived.tagsAI, identifier: identifier, seed: seed) {
                tags.remove(tag)
            }
        }
        return tags
    }

    // MARK: Filters

    /// Whether an asset's *facets* satisfy the query's non-visibility filters.
    ///
    /// Visibility is deliberately not checked here: the enumeration already
    /// picks the right population, so re-testing the three exclusion flags would
    /// be a second implementation of the rule most likely to drift.
    func matchesFacets(_ query: TimelineQuery, facets: MockAssetFacets, patch: MockAssetPatch?) -> Bool {
        if let kind = query.mediaKind, facets.mediaKind != kind { return false }
        if let after = query.capturedAfter, facets.captureUTC < after.epochSeconds { return false }
        if let before = query.capturedBefore, facets.captureUTC > before.epochSeconds { return false }
        let cull = patch?.cull ?? facets.cull
        if let wanted = query.cull, cull != wanted { return false }
        let rating = patch?.rating ?? facets.rating
        if let minimum = query.minimumRating, rating < minimum { return false }
        if let albumID = query.albumID {
            let resolved = patch?.albumID
                ?? MockIdentifiers.albumID(seed: seed, ordinal: facets.albumOrdinal)
            if resolved != albumID { return false }
        }
        return true
    }

    /// Whether the query applies no facet filter at all — the case an
    /// unfiltered timeline takes, and the one that must stay O(days).
    func hasNoFacetFilter(_ query: TimelineQuery) -> Bool {
        query.albumID == nil && query.mediaKind == nil && query.capturedAfter == nil
            && query.capturedBefore == nil && query.cull == nil && query.minimumRating == nil
    }

    /// Whether a live base index is still live — the user may have trashed,
    /// hidden, or purged it since.
    func isSuppressed(liveIndex: Int) -> Bool {
        let identifier = MockAssetRef(kind: .live, index: liveIndex).identifier(seed: seed)
        guard let patch = overlay.patch(for: identifier) else { return false }
        return patch.isPurged || patch.isDeleted == true || patch.isUserHidden == true
    }
}

// MARK: - MockTagIdentity

/// The add ids behind derived tags.
///
/// The derived library has no stored OR-set, but ``OrganizePort`` removes a tag
/// **by the add id that introduced it** — and a remove naming an add this
/// replica never observed must be *rejected*, not ignored. Deriving the add id
/// from `(asset, tag)` gives every derived tag a stable identity to name, so the
/// rejection path is real rather than decorative.
public enum MockTagIdentity {
    /// The device every derived tag is attributed to.
    public static func authoringDevice(seed: UInt64) -> DeviceID {
        MockIdentifiers.deviceID(seed: seed, ordinal: 0)
    }

    public static func addID(forTag tag: String, identifier: AssetID, seed: UInt64, isAI: Bool) -> AddID {
        var accumulator: UInt64 = isAI ? 0x9E37 : 0x1F3B
        for byte in tag.utf8 {
            accumulator = MockHash.mix(accumulator &+ UInt64(byte))
        }
        for byte in identifier.sortKey.utf8 {
            accumulator = MockHash.mix(accumulator &+ UInt64(byte))
        }
        return AddID(deviceID: authoringDevice(seed: seed), counter: accumulator % 900000)
    }

    static func userTag(
        matching addID: AddID,
        in tags: Set<String>,
        identifier: AssetID,
        seed: UInt64
    ) -> String? {
        tags.first { self.addID(forTag: $0, identifier: identifier, seed: seed, isAI: false) == addID }
    }

    static func aiTag(
        matching addID: AddID,
        in tags: Set<AiTag>,
        identifier: AssetID,
        seed: UInt64
    ) -> AiTag? {
        tags.first { self.addID(forTag: $0.tag, identifier: identifier, seed: seed, isAI: true) == addID }
    }
}
