import CapsuleCatalog
import CapsuleFoundation
import Foundation

// MARK: - CompositeTrashProvider

/// One Recently Deleted over several soft-deleting stores.
///
/// The timeline is a ``CompositeAssetProvider`` whose `delete` fans out to every
/// source, so an asset can be trashed in a store the trash screen never lists.
/// This is the trash-side counterpart: it lists every source's trash as one,
/// and routes a restore or purge to the store that owns the asset.
///
/// ## One challenge opens every source (SR1)
///
/// The **primary** holds the gate the user answers. Each **secondary** is built
/// over a ``SecondaryTrashGate`` that opens only for the single challenge this
/// type admits after the primary's grant is live, so unlocking costs the user
/// one prompt however many stores are behind it, and no secondary can open on
/// its own. ``isTrashUnlocked()`` is the conjunction, so an expired grant on any
/// source sends the screen back through ``unlockTrash()``, which re-opens only
/// the sources that need it.
///
/// ## Routing
///
/// A restore or purge goes to the first secondary that ``AssetOwning/owns(_:)``
/// the asset, and otherwise to the primary. Secondaries are asked first because
/// their ownership answer is exact (a catalog row) where the primary's — the
/// mock library — is not.
public struct CompositeTrashProvider: TrashProvider {
    private let primary: any TrashProvider
    private let secondaries: [any TrashProvider & AssetOwning]
    private let secondaryGate: SecondaryTrashGate

    /// - Parameters:
    ///   - primary: the source whose gate challenges the user, and the owner of
    ///     every asset no secondary claims.
    ///   - secondaries: sources built over `secondaryGate`.
    ///   - secondaryGate: the gate every secondary authenticates through.
    public init(
        primary: any TrashProvider,
        secondaries: [any TrashProvider & AssetOwning],
        secondaryGate: SecondaryTrashGate
    ) {
        self.primary = primary
        self.secondaries = secondaries
        self.secondaryGate = secondaryGate
    }

    /// Every source's trash, newest capture first.
    ///
    /// A source that cannot list — locked, or failing to open — fails the whole
    /// listing rather than being skipped: a Recently Deleted that silently left
    /// a source out would hide assets the user is deciding whether to destroy.
    ///
    /// Ordered by capture date because that is the one order every source can
    /// answer in: ``Asset`` carries no deletion time.
    public func trashedAssets() async throws -> [Asset] {
        var assets = try await primary.trashedAssets()
        for secondary in secondaries {
            try await assets.append(contentsOf: secondary.trashedAssets())
        }
        assets.sort { $0.captureDate > $1.captureDate }
        CapsuleLog.assetKit.debug("composite trash listed \(assets.count, privacy: .public) assets")
        return assets
    }

    public func restore(_ id: AssetID) async throws {
        try await owner(of: id).restore(id)
    }

    public func purge(_ id: AssetID) async throws {
        try await owner(of: id).purge(id)
    }

    /// Challenge through the primary, then open each secondary on that grant.
    ///
    /// The primary's refusal is thrown unchanged and no secondary is touched.
    public func unlockTrash() async throws {
        try await primary.unlockTrash()
        for secondary in secondaries where await !secondary.isTrashUnlocked() {
            secondaryGate.admitNextChallenge()
            defer { secondaryGate.revoke() }
            try await secondary.unlockTrash()
        }
        CapsuleLog.assetKit.debug("composite trash unlocked across \(secondaries.count + 1, privacy: .public) sources")
    }

    public func isTrashUnlocked() async -> Bool {
        guard await primary.isTrashUnlocked() else { return false }
        for secondary in secondaries where await !secondary.isTrashUnlocked() {
            return false
        }
        return true
    }

    // MARK: Private

    private func owner(of id: AssetID) async -> any TrashProvider {
        for secondary in secondaries where await secondary.owns(id) {
            return secondary
        }
        return primary
    }
}

// MARK: - SecondaryTrashGate

/// The ``LocalAuthGate`` a secondary trash source is built over.
///
/// It challenges nobody. It opens for exactly one challenge, and only after
/// ``CompositeTrashProvider`` has seen the primary's grant go live and admitted
/// it; any other challenge is refused with ``LocalAuthError/failed``. A
/// secondary built over it therefore cannot be opened by anything but the
/// composite, which is what keeps one prompt covering every source without
/// leaving a source that opens unprompted.
public final class SecondaryTrashGate: LocalAuthGate, @unchecked Sendable {
    /// Guards ``admitted``; the gate's protocol is synchronous, so an actor
    /// cannot hold it.
    private let lock = NSLock()
    private var admitted = false

    public init() {}

    /// Open for the next challenge only.
    func admitNextChallenge() {
        lock.lock()
        admitted = true
        lock.unlock()
    }

    /// Close again — a grant reused without a challenge leaves the admission
    /// unspent, and it must not outlive the unlock that made it.
    func revoke() {
        lock.lock()
        admitted = false
        lock.unlock()
    }

    public func authenticate(view _: GatedView) throws {
        lock.lock()
        let open = admitted
        admitted = false
        lock.unlock()
        guard open else {
            CapsuleLog.assetKit.error("secondary trash gate challenged without a primary grant")
            throw LocalAuthError.failed
        }
    }
}
