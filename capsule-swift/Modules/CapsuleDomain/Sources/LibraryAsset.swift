import CapsuleFoundation
import Foundation

// MARK: - LibraryAsset

/// One row of the timeline — the projection every grid, viewer, and picker
/// binds to.
///
/// Deliberately **not** a ``SidecarV1``. The sidecar is the signed wire record
/// and carries CRDT registers, unknown bytes, and identifiers a view has no
/// business touching; this is the flattened, already-resolved shape a list cell
/// needs, produced once at the adapter boundary. Keeping them separate means a
/// view can never accidentally author a sidecar field, and the projection can
/// change without touching a signed schema.
///
/// ## The three flags that are not the same flag
///
/// This is the single most important thing about this type. An asset can be
/// absent from the timeline for three unrelated reasons, and conflating any two
/// of them produces a bug that looks like data loss:
///
/// - ``isDeleted`` — the asset is in the **trash**, inside a signed retention
///   window. It is recoverable, it still counts fully against quota, and it
///   appears in the Trash view.
/// - ``isStackHidden`` — the asset is a **non-cover member of a collapsed
///   stack**. It is perfectly live and visible the moment the stack is
///   expanded; nothing was hidden or deleted.
/// - ``isUserHidden`` — the user **hid** it. It is excluded from default views
///   and appears only in the Hidden view, behind the same fresh-local-auth gate
///   as trash. It still syncs and stays in its album.
///
/// Only ``isUserHidden`` is a CRDT register on the sidecar; the other two are
/// derived lifecycle and grouping facts. A query that filters on the wrong one
/// either resurrects deleted photos or makes live ones vanish.
public struct LibraryAsset: Sendable, Equatable, Identifiable, Hashable {
    /// The source-tagged identifier every per-asset request round-trips through.
    public var id: AssetID
    /// The presentation classification — which viewer and which badge.
    public var mediaType: MediaType
    /// The asset's format, from the sidecar.
    public var contentType: ContentType
    /// Capture instants, both conventions. Sort on
    /// ``CaptureTime/effectiveCaptureTimestamp``.
    public var captureTime: CaptureTime
    /// When it entered the library.
    public var importTimestamp: CapsuleTimestamp
    /// Pixel dimensions, when known. Drives grid aspect ratio.
    public var dimensions: Dimensions?
    /// The embedded placeholder — the bottom of the degrade ladder, and why a
    /// tile is never blank.
    public var lqip: Lqip?
    /// Duration for time-based media.
    public var durationMilliseconds: Int64?

    /// Resolved culling flag (``CullFlag/neutral`` when never flagged).
    public var cull: CullFlag
    /// Resolved star rating, 0–5. Orthogonal to ``cull``.
    public var rating: UInt8
    /// Resolved user tags.
    public var tagsUser: Set<String>
    /// Resolved AI tags, with their model slots intact — a term over a stale
    /// slot must be excludable, which requires the slot to survive the
    /// projection.
    public var tagsAI: Set<AiTag>
    /// The current caption, when set.
    public var caption: String?
    /// Whether a displaced caption exists to offer restoring.
    public var hasSupersededCaptions: Bool

    /// Stack membership, when stacked.
    public var stackMembership: StackMembership?
    /// The container album this asset lives in. Exactly one, always.
    public var albumID: AlbumID?

    /// **Trash.** Soft-deleted, inside a retention window.
    public var isDeleted: Bool
    /// When it was soft-deleted.
    public var deletedAt: CapsuleTimestamp?
    /// **Stack collapse.** A non-cover member of a collapsed stack.
    public var isStackHidden: Bool
    /// **User-hidden.** Excluded from default views by explicit user action.
    public var isUserHidden: Bool

    /// Which rungs of the ladder this device holds.
    public var representations: LocalRepresentations
    /// Where the asset stands between this device and the server.
    public var syncState: AssetSyncState

    public init(
        id: AssetID,
        mediaType: MediaType,
        contentType: ContentType,
        captureTime: CaptureTime,
        importTimestamp: CapsuleTimestamp,
        dimensions: Dimensions? = nil,
        lqip: Lqip? = nil,
        durationMilliseconds: Int64? = nil,
        cull: CullFlag = .neutral,
        rating: UInt8 = 0,
        tagsUser: Set<String> = [],
        tagsAI: Set<AiTag> = [],
        caption: String? = nil,
        hasSupersededCaptions: Bool = false,
        stackMembership: StackMembership? = nil,
        albumID: AlbumID? = nil,
        isDeleted: Bool = false,
        deletedAt: CapsuleTimestamp? = nil,
        isStackHidden: Bool = false,
        isUserHidden: Bool = false,
        representations: LocalRepresentations = LocalRepresentations(),
        syncState: AssetSyncState = .durable
    ) {
        self.id = id
        self.mediaType = mediaType
        self.contentType = contentType
        self.captureTime = captureTime
        self.importTimestamp = importTimestamp
        self.dimensions = dimensions
        self.lqip = lqip
        self.durationMilliseconds = durationMilliseconds
        self.cull = cull
        self.rating = rating
        self.tagsUser = tagsUser
        self.tagsAI = tagsAI
        self.caption = caption
        self.hasSupersededCaptions = hasSupersededCaptions
        self.stackMembership = stackMembership
        self.albumID = albumID
        self.isDeleted = isDeleted
        self.deletedAt = deletedAt
        self.isStackHidden = isStackHidden
        self.isUserHidden = isUserHidden
        self.representations = representations
        self.syncState = syncState
    }

    /// **The timeline axis.** The UTC capture instant when known, else the
    /// device-local wall clock. Every sort, section boundary, and date filter
    /// reads this.
    public var effectiveCaptureTimestamp: CapsuleTimestamp {
        captureTime.effectiveCaptureTimestamp
    }

    /// The UTC day this asset sections into.
    public var dayKey: DayKey {
        effectiveCaptureTimestamp.dayKey
    }

    /// Whether the asset belongs in the **default** timeline.
    ///
    /// All three exclusion flags, applied in one place, so no view has to
    /// remember which is which.
    public var appearsInDefaultTimeline: Bool {
        !isDeleted && !isStackHidden && !isUserHidden
    }

    /// Whether the asset is the cover of a collapsed stack.
    public var isStackCover: Bool {
        stackMembership?.isStackCover ?? false
    }
}

// MARK: - Ordering

public extension LibraryAsset {
    /// The canonical newest-first order.
    ///
    /// Tie-broken on the asset identifier, exactly as the aggregated album's
    /// merge order is. The tiebreak is not cosmetic: without it, two assets
    /// captured in the same second can order differently on two devices, and a
    /// grid's section offsets stop agreeing across the account.
    static func isOrderedNewestFirst(_ lhs: LibraryAsset, _ rhs: LibraryAsset) -> Bool {
        if lhs.effectiveCaptureTimestamp != rhs.effectiveCaptureTimestamp {
            return lhs.effectiveCaptureTimestamp > rhs.effectiveCaptureTimestamp
        }
        return lhs.stableSortKey < rhs.stableSortKey
    }

    /// A total, device-independent tiebreak over the identifier.
    var stableSortKey: String {
        switch id {
        case let .photoKit(localIdentifier): "photokit:\(localIdentifier)"
        case let .managed(uuid): "managed:\(uuid)"
        }
    }
}

// MARK: - TimelineQuery

/// Which lifecycle-and-visibility slice of the library a query selects.
///
/// A **slice, not a set of "include" toggles.** Trash and Hidden are system
/// views *over* lifecycle and visibility state, so they select only their own
/// assets; an "includeDeleted" flag would make the Trash view show the whole
/// library with the trash mixed in. Modelling it as one closed choice makes
/// that mistake unrepresentable.
public enum VisibilitySlice: Sendable, Equatable, Hashable, CaseIterable {
    /// The default timeline: not trashed, not user-hidden.
    case live
    /// Only soft-deleted assets, inside their retention window.
    case trash
    /// Only user-hidden assets. Behind the same fresh-local-auth gate as trash.
    case userHidden
}

/// The facets applied to a paged library read.
///
/// Every optional facet `nil` means "not applied", so a query with the default
/// slice and no facets is the full default timeline. Modelled as one struct
/// rather than a dozen port methods, because the alternative is a combinatorial
/// explosion of `assetsInAlbumRatedAtLeastCapturedBetween(…)`.
public struct TimelineQuery: Sendable, Equatable, Hashable {
    /// Which lifecycle slice to select.
    public var slice: VisibilitySlice
    /// Restrict to one container album or view.
    public var albumID: AlbumID?
    /// Restrict to one media kind.
    public var mediaKind: MediaKind?
    /// Inclusive lower bound on ``LibraryAsset/effectiveCaptureTimestamp``.
    public var capturedAfter: CapsuleTimestamp?
    /// Inclusive upper bound on ``LibraryAsset/effectiveCaptureTimestamp``.
    public var capturedBefore: CapsuleTimestamp?
    /// Restrict to one culling flag — the filter a review pass runs on.
    public var cull: CullFlag?
    /// Inclusive lower bound on the star rating.
    public var minimumRating: UInt8?
    /// Include non-cover members of collapsed stacks. An expanded stack sets
    /// this; the default timeline does not.
    ///
    /// Orthogonal to ``slice``, because stack collapse is a *grouping* fact,
    /// not a lifecycle one: a stack member can be collapsed and trashed, or
    /// collapsed and live.
    public var includeStackHidden: Bool

    public init(
        slice: VisibilitySlice = .live,
        albumID: AlbumID? = nil,
        mediaKind: MediaKind? = nil,
        capturedAfter: CapsuleTimestamp? = nil,
        capturedBefore: CapsuleTimestamp? = nil,
        cull: CullFlag? = nil,
        minimumRating: UInt8? = nil,
        includeStackHidden: Bool = false
    ) {
        self.slice = slice
        self.albumID = albumID
        self.mediaKind = mediaKind
        self.capturedAfter = capturedAfter
        self.capturedBefore = capturedBefore
        self.cull = cull
        self.minimumRating = minimumRating
        self.includeStackHidden = includeStackHidden
    }

    /// The unfiltered default timeline.
    public static let `default` = TimelineQuery()

    /// The Trash view's query — trashed assets only.
    public static let trash = TimelineQuery(slice: .trash)

    /// The Hidden view's query — user-hidden assets only.
    public static let hidden = TimelineQuery(slice: .userHidden)

    /// Whether an asset falls inside this query's slice.
    ///
    /// Exposed so a mock and the real adapter cannot disagree about the three
    /// flags — the one rule most likely to drift between implementations, and
    /// the one whose drift looks exactly like data loss.
    ///
    /// Stack-collapse is applied **after** the slice, because it is a grouping
    /// fact rather than a lifecycle one: an expanded stack inside the Trash view
    /// should show its trashed members, not resurrect its live ones.
    public func admitsVisibility(of asset: LibraryAsset) -> Bool {
        switch slice {
        case .live:
            guard !asset.isDeleted, !asset.isUserHidden else { return false }
        case .trash:
            guard asset.isDeleted else { return false }
        case .userHidden:
            guard asset.isUserHidden, !asset.isDeleted else { return false }
        }
        return includeStackHidden || !asset.isStackHidden
    }
}
