import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - TimelineFocus

/// How densely the timeline groups what it shows.
///
/// A *zoom level*, not a filter: every focus shows the same assets, gathered
/// into progressively larger buckets. It is part of the route rather than view
/// state because it is what the ⌘1…⌘4 menu commands change, what a restored
/// session must come back to, and what the back button must return you to.
public enum TimelineFocus: String, Sendable, Hashable, Codable, CaseIterable {
    /// Every asset, ungrouped — the flat grid.
    case all
    /// Grouped by capture day.
    case days
    /// Grouped by capture month.
    case months
    /// Grouped by capture year — the widest zoom.
    case years
}

// MARK: - Route

/// Every destination in the app, as one closed, addressable value.
///
/// ## Why one enum for three shells
///
/// The app ships an iPhone tab bar, an iPad/Mac three-column split view,
/// detached Mac windows, and a Mac menu bar. Each of those is a different way
/// of *arranging* destinations; none of them is a different set of
/// destinations. Giving each shell its own navigation vocabulary would mean
/// every deep link, every menu command, and every notification handler is
/// written three times and drifts three ways. `Route` is the single vocabulary
/// they all speak — the shells differ only in ``Route/preferredColumn`` and in
/// which ``SidebarItem`` surfaces they expose.
///
/// ## Why the payloads are identifiers, never models
///
/// A route is persisted for state restoration and carried across window
/// boundaries, so it must stay small, `Sendable`, and stable across a library
/// that changed while the app was gone. Every payload is therefore an
/// identifier or a query — never a fetched model — and the screen re-resolves
/// it on appearance. A route that embedded a `LibraryAsset` would restore a
/// stale copy of a photo the user has since edited.
///
/// ## Why it is `Codable`
///
/// Scene restoration on both platforms round-trips the navigation stack
/// through `Codable`. That is a hard requirement rather than a nicety: without
/// it, every relaunch drops the user at a section root. `RouteCodableTests`
/// pins the round-trip for every case.
///
/// > Note: routes deliberately carry **no secrets**. A share link's fragment
/// > never becomes part of a route — see ``DeepLink`` and ``LinkSecret`` for
/// > why, and for where the secret goes instead.
public enum Route: Sendable, Hashable, Codable {
    // MARK: Library

    /// The main timeline at a given zoom.
    case timeline(TimelineFocus)
    /// The generated memories shelf.
    case memories
    /// The duplicate-review surface.
    case duplicates
    /// Soft-deleted assets inside their retention window.
    case trash
    /// User-hidden assets, behind the fresh-local-auth gate.
    case hidden
    /// One asset, full screen, paging within `context`.
    ///
    /// The context is what makes the viewer able to swipe *past* the assets
    /// currently in memory: it names the sequence, so the viewer can ask the
    /// provider for the next page rather than being trapped inside a snapshot.
    case viewer(AssetID, context: ViewerContext)
    /// The keyboard-driven cull pass over a sequence.
    case culling(ViewerContext)

    // MARK: Collections

    /// The phone's index of every section its tab bar does not carry.
    ///
    /// Deliberately has no deep-link grammar: a URL names content, and this
    /// names a shell's table of contents.
    case browse
    /// The album index — user albums and smart albums together.
    case albums
    /// One album's contents.
    case album(AlbumID)
    /// One album's participant list.
    case albumMembers(AlbumID)
    /// One album's sharing and retention policy.
    case albumPolicy(AlbumID)
    /// One smart album's results.
    case smartAlbum(SmartAlbumID)
    /// The smart-album predicate editor; `nil` creates a new definition.
    case smartAlbumEditor(SmartAlbumID?)
    /// The people index.
    case people
    /// One person cluster.
    case person(PersonID)
    /// The places map.
    case places
    /// The assets inside one map bounding box.
    case place(MapRegion)
    /// A search, over `scope`, for `text`.
    ///
    /// The text is part of the route because `capsule://search?q=…` has to land
    /// somewhere and a scope alone cannot express a query. `nil` text is the
    /// empty search field — the state the Search tab opens in.
    case search(SearchScope, text: String?)

    // MARK: Transfer and provenance

    /// The aggregate view of in-flight uploads and downloads.
    case transferCenter
    /// One asset's upload progress and retry history.
    case uploadDetail(AssetID)
    /// The signed custody receipt proving the server took durable delivery.
    case custodyReceipt(AssetID)
    /// The import history.
    case imports
    /// One import run's candidates, decisions, and outcome.
    case importSession(ImportID)
    /// The quarantine inventory.
    case quarantine
    /// One quarantined item and its resolution options.
    case quarantineItem(QuarantineID)

    // MARK: Sharing

    /// Share links this library has issued.
    case shares
    /// One issued share link, for inspection and revocation.
    case shareDetail(ShareID)
    /// The inbox of guest uploads awaiting adoption.
    case drops
    /// One pending drop.
    case drop(DropID)
    /// Redemption of an inbound `https` link — someone else's share, or a
    /// guest-upload invitation.
    ///
    /// Carries only the URL's *opaque* path id. The fragment secret travels
    /// beside the route in a ``LinkSecret`` and is never persisted, so a
    /// restored session cannot resurrect a key the user has since revoked.
    case linkRedemption(InvitationKind, opaqueID: String)

    // MARK: Fleet and federation

    /// The enrolled-device directory.
    case devices
    /// Known federated peers.
    case peers
    /// Federation posture: budgets, breakers, moderation state.
    case federation

    // MARK: Storage and system

    /// Quota headroom and its projection.
    case quota
    /// Local and remote storage occupancy.
    case storage
    /// The maintenance surface; a non-`nil` kind opens that task's detail.
    case maintenance(MaintenanceTaskKind?)
    /// One settings screen.
    case settings(SettingsSection)
    /// One step of first-run setup.
    case onboarding(OnboardingStep)
}
