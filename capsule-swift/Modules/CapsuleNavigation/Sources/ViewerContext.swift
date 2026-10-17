import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - ViewerContext

/// The *sequence* a full-screen viewer is paging through.
///
/// ## Why this is a query and not a list of assets
///
/// The obvious design — hand the viewer the assets the grid had loaded — breaks
/// the moment the user swipes past the loaded window: the viewer either stops
/// at an arbitrary boundary or has to reach back into a view it does not own.
/// Naming the *sequence* instead means the viewer can ask the provider for
/// "the asset after this one, in this order" indefinitely, and the answer stays
/// correct while the library changes underneath it.
///
/// It is also what makes the viewer restorable and window-portable. A snapshot
/// of forty `LibraryAsset` values cannot be persisted or sent to a detached Mac
/// window; a query can. The same reasoning is why ``Route`` payloads are
/// identifiers rather than models.
///
/// ## Why it is separate from `Route`
///
/// Several routes share one sequence — the grid, its viewer, and a cull pass
/// over it are three destinations over the same ordering — and a sequence has
/// to survive being handed from one to the next. Keeping it a standalone value
/// also lets ``Router/currentViewerContext`` answer "what would a cull review
/// operate on right now?" without pattern-matching every route case at the call
/// site.
public enum ViewerContext: Sendable, Hashable, Codable {
    /// The timeline under an arbitrary query — the general case, and the one
    /// that carries Trash and Hidden by way of ``VisibilitySlice``.
    case timeline(TimelineQuery)
    /// One album's ordering.
    case album(AlbumID)
    /// One smart album's evaluated results.
    case smartAlbum(SmartAlbumID)
    /// Everything attributed to one person cluster.
    case person(PersonID)
    /// Everything inside one map bounding box.
    case place(MapRegion)
    /// A search result set. Re-runs rather than replays, so a viewer opened
    /// from search still pages correctly after new assets are indexed.
    case search(text: String, scope: SearchScope)
    /// The memories shelf.
    case memories
    /// A duplicate cluster under review.
    case duplicates
}

public extension ViewerContext {
    /// The default sequence: the whole live library.
    ///
    /// Used wherever a viewer is opened without an originating collection — a
    /// `capsule://asset/…` deep link, most obviously, where the link names an
    /// asset and nothing about how the user got to it.
    static let library = ViewerContext.timeline(.default)

    /// The sidebar section this sequence belongs to.
    ///
    /// Load-bearing for ``Router/select(_:)``: opening a viewer must switch to
    /// the section that owns the *sequence*, not to whichever section happened
    /// to be showing. Deep-linking into a trashed asset belongs in Trash.
    var owningSection: SidebarItem {
        switch self {
        case let .timeline(query): Self.section(for: query.slice)
        case .album, .smartAlbum: .albums
        case .person: .people
        case .place: .places
        case .search: .search
        case .memories: .memories
        case .duplicates: .duplicates
        }
    }

    /// Trash and Hidden are lifecycle *slices* of the timeline rather than
    /// separate queries, so the section they belong to is decided here rather
    /// than being another pair of `ViewerContext` cases.
    private static func section(for slice: VisibilitySlice) -> SidebarItem {
        switch slice {
        case .live: .library
        case .trash: .trash
        case .userHidden: .hidden
        }
    }
}
