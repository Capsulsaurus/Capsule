import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockLibraryProfile

/// Everything a scenario varies about the *shape* of the synthetic library.
///
/// A value type rather than a set of `MockScenario` switches inside the
/// derivation, because the derivation must stay a pure function of
/// `(profile, index)`. Anything the derivation reads has to arrive through here,
/// which is also what makes a test able to construct a library with, say, one
/// day and four assets and get exactly the same code path the 250 000-asset one
/// takes.
public struct MockLibraryProfile: Sendable, Equatable, Hashable {
    /// The world seed. Two profiles differing only in seed produce two
    /// completely different but equally plausible libraries.
    public var seed: UInt64
    /// How many assets are in the default timeline. Not a memory cost — see
    /// ``MockLibrary``.
    public var assetCount: Int
    /// How many UTC days the captures are spread over.
    public var spanDays: Int
    /// The UTC day number of the newest day with photos.
    public var newestDayNumber: Int64

    /// Per-mille of assets whose original is still on the device that took it.
    /// A **badge, not a failure** — see ``AssetSyncState/awaitingOriginal(heldBy:)``.
    public var awaitingOriginalPerMille: Int
    /// Per-mille of assets this build cannot open — no codec, or corrupt local
    /// bytes.
    public var unreadablePerMille: Int
    /// Per-mille of assets carrying closed-enum values and a schema this build
    /// does not know, so the "created with a newer version" indicator and the
    /// disabled-editing path are reachable.
    public var newerVersionPerMille: Int
    /// Per-mille of assets held in quarantine.
    public var quarantinedPerMille: Int

    /// Whether remote-only rungs of the degrade ladder are unavailable.
    ///
    /// Set by ``MockScenario/offline``. It degrades what is *drawn*; it must
    /// never stop a local read from answering, because the offline-first
    /// contract is that a gallery read never attempts the network.
    public var degradesRemoteRepresentations: Bool

    public init(
        seed: UInt64 = 0x0C0F_FEE0_1234_5678,
        assetCount: Int = 4000,
        spanDays: Int = 2190,
        newestDayNumber: Int64,
        awaitingOriginalPerMille: Int = 20,
        unreadablePerMille: Int = 0,
        newerVersionPerMille: Int = 0,
        quarantinedPerMille: Int = 0,
        degradesRemoteRepresentations: Bool = false
    ) {
        self.seed = seed
        self.assetCount = max(0, assetCount)
        self.spanDays = max(1, spanDays)
        self.newestDayNumber = newestDayNumber
        self.awaitingOriginalPerMille = awaitingOriginalPerMille
        self.unreadablePerMille = unreadablePerMille
        self.newerVersionPerMille = newerVersionPerMille
        self.quarantinedPerMille = quarantinedPerMille
        self.degradesRemoteRepresentations = degradesRemoteRepresentations
    }

    /// How many soft-deleted assets the trash holds before the user touches it.
    ///
    /// Bounded rather than proportional: a trash view is a list a person reads,
    /// and 250 000 assets do not imply 5 000 deleted ones.
    public var derivedTrashCount: Int {
        assetCount == 0 ? 0 : min(48, max(4, assetCount / 120))
    }

    /// How many user-hidden assets exist before the user hides anything.
    public var derivedHiddenCount: Int {
        assetCount == 0 ? 0 : min(16, max(2, assetCount / 400))
    }
}
