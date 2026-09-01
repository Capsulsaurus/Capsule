import CapsuleCatalog
import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - PortBackedTrashProvider

/// The ``TrashProvider`` the Recently Deleted screen sees, over
/// ``OrganizePort``.
///
/// ## Why the listing does not come from `trashEntries`
///
/// ``OrganizePort/trashEntries(offset:limit:)`` is the natural-looking source,
/// but a ``TrashEntry`` carries only `(assetID, deletedAt, retentionUntil)` —
/// no media type, no dimensions, no capture date — so turning a page of them
/// into `[Asset]` would mean a second per-asset read anyway. Reading the trash
/// **slice** of the timeline instead (``VisibilitySlice/trash``) returns whole
/// ``LibraryAsset`` rows in one window, already ordered, and goes through the
/// same query path the rest of the app uses — so the trash view cannot drift
/// from the timeline's own idea of what is deleted.
///
/// `trashEntries` remains the right read for a screen that wants to show the
/// **signed retention deadline**, which is the one fact only it carries.
/// `TrashProvider` has nowhere to put a deadline, so this adapter does not
/// pretend to expose one.
///
/// ## The SR1 gate, without a core to hold it
///
/// The FFI lane's grant lives in Rust: `Catalog` refuses `queryTrash` until
/// `unlockView` has minted one, and the grace clock is the core's. The port
/// surface has no such gate — nothing behind ``LibraryPort`` knows what fresh
/// local authentication is — so this adapter holds the grant itself, over the
/// same ``LocalAuthenticator`` seam the Hidden and Security screens use and with
/// the same 5-minute per-view window. Both lanes therefore answer
/// ``isTrashUnlocked()`` and ``unlockTrash()`` the same way, and Recently
/// Deleted needs no idea which one it is talking to.
public struct PortBackedTrashProvider: TrashProvider {
    /// Upper bound on the trash listing. `TrashProvider` returns a materialised
    /// array, so the bound is explicit rather than implied.
    public static let maximumTrashedAssets = 2000

    /// The SR1 grace window, in seconds — the same 5 minutes the core enforces
    /// (`capsule_core::library::DEFAULT_GRACE`). Duplicated as a constant rather
    /// than read from the core, because in this lane there is no core.
    public static let graceSeconds: TimeInterval = 300

    private let organize: any OrganizePort
    private let library: any LibraryPort
    private let authenticator: any LocalAuthenticator
    private let grant: TrashGrant

    public init(
        organize: any OrganizePort,
        library: any LibraryPort,
        authenticator: any LocalAuthenticator
    ) {
        self.organize = organize
        self.library = library
        self.authenticator = authenticator
        grant = TrashGrant(windowSeconds: Self.graceSeconds)
    }

    /// Every soft-deleted asset, newest first.
    ///
    /// Refuses with `CatalogError.viewLocked` unless a live grant exists, which
    /// is what the Rust-backed lane does — a mock that served the trash freely
    /// would let the screen forget it is gated at all.
    ///
    /// The port's canonical order is newest **capture** first, not
    /// most-recently-deleted first as `TrashProvider` documents. The two agree
    /// for the common case and the difference is not worth a full-slice sort
    /// through a second read; a screen that needs deletion order should read
    /// ``OrganizePort/trashEntries(offset:limit:)``, which carries `deletedAt`.
    public func trashedAssets() async throws -> [Asset] {
        guard await grant.isLive(at: Date()) else { throw CatalogError.viewLocked }
        let page = try await library.assets(
            matching: .trash,
            offset: 0,
            limit: Self.maximumTrashedAssets
        )
        return page.items.map(Asset.init(libraryAsset:))
    }

    /// Restore from trash, inside the retention window.
    ///
    /// Appends a provenance record; the original `delete` record survives, so
    /// the chain keeps "deleted on X, restored on Y".
    public func restore(_ id: AssetID) async throws {
        try await organize.restoreFromTrash([id])
    }

    /// Purge ahead of the signed retention deadline, at the user's explicit
    /// request. Irreversible for the bytes; the provenance chain survives as a
    /// tombstone-with-history.
    public func purge(_ id: AssetID) async throws {
        try await organize.purge([id])
    }

    // MARK: The gate

    /// Take the grant, challenging only when one is not already live.
    ///
    /// A refusal is thrown, not swallowed: the screen distinguishes "the user
    /// said no" from "there is nothing in the trash", and only an error can
    /// carry that. A cancel arrives as ``LocalAuthError/cancelled``.
    public func unlockTrash() async throws {
        let now = Date()
        if await grant.isLive(at: now) { return }
        // A device with no credential at all opens rather than sealing shut:
        // refusing would make Recently Deleted permanently unreachable while
        // protecting nothing, which is what `HiddenView` already decided.
        if await authenticator.availableMethod() == .unavailable {
            await grant.mint(at: now)
            return
        }
        guard try await authenticator.authenticate(
            reasonKey: "app.recently_deleted.auth.reason"
        ) else {
            throw LocalAuthError.cancelled
        }
        await grant.mint(at: now)
    }

    public func isTrashUnlocked() async -> Bool {
        await grant.isLive(at: Date())
    }
}

// MARK: - TrashGrant

/// The one piece of mutable state this adapter owns: when the trash grant was
/// minted.
///
/// An actor so the `struct` above can stay a value type — the adapter is copied
/// into views, and every copy must see the same grant or the window would reset
/// on each navigation.
///
/// The window is **not** slid forward by a reuse: it runs from the original
/// mint, exactly as the core's `GateKeeper::open` does, so re-entering the
/// screen cannot hold it open indefinitely.
private actor TrashGrant {
    private let windowSeconds: TimeInterval
    private var mintedAt: Date?

    init(windowSeconds: TimeInterval) {
        self.windowSeconds = windowSeconds
    }

    func mint(at now: Date) {
        mintedAt = now
    }

    func isLive(at now: Date) -> Bool {
        guard let mintedAt else { return false }
        return now.timeIntervalSince(mintedAt) < windowSeconds
    }
}
